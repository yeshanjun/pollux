use super::{
    ops::CredentialOps,
    scheduler::{CredentialId, CredentialManager},
};
use crate::config::CodexResolvedConfig;
use crate::db::CodexPatch;
use crate::error::{OauthError, PolluxError};
use crate::model_catalog::MODEL_REGISTRY;
use crate::providers::codex::resource::CodexResource;
use crate::providers::codex::{
    CodexRefreshTokenSeed, SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES, oauth::OauthTokenResponse,
};
use crate::providers::manifest::CodexLease;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, warn};

use super::super::{CodexRefresherHandle, RefreshOutcome};

/// Public messages handled by the Codex actor.
#[derive(Debug)]
pub enum CodexActorMessage {
    /// Request one available credential for the given model mask. Returns `None` if none available.
    GetCredential(u64, RpcReplyPort<Option<CodexLease>>),

    /// Report rate limiting; start a per-model cooldown for this credential.
    ReportRateLimit {
        id: CredentialId,
        model_mask: u64,
        cooldown: Duration,
    },

    /// Report unsupported model (e.g. 400/404); clear capability bits for this credential.
    ReportModelUnsupported { id: CredentialId, model_mask: u64 },

    /// Report invalid/expired access (e.g. 401); refresh then re-enqueue.
    ReportInvalid { id: CredentialId },

    /// Report a credential as banned/unusable; remove from queues and storage.
    ReportBaned { id: CredentialId },

    /// Submit a trusted OAuth token response (from the server-side OAuth exchange).
    ///
    /// This should already contain access_token + expiry + id_token. The actor will decode
    /// identity from id_token, persist into DB, then activate in memory.
    SubmitTrustedOauth(OauthTokenResponse),

    /// Submit untrusted refresh token seeds and trigger one refresh pass for each.
    ///
    /// This is intended for 0-trust ingestion (e.g. an add-credentials endpoint). The actor will
    /// only persist+activate after a refresh succeeds and identity can be derived.
    SubmitUntrustedSeeds(Vec<CodexRefreshTokenSeed>),

    // Internal messages (sent by the actor itself / workers)
    /// Token refresh has completed; update stored credential and re-enqueue if ok.
    RefreshComplete { outcome: RefreshOutcome },
    /// A credential has been refreshed and stored; activate it in memory queues.
    ActivateCredential {
        id: CredentialId,
        credential: CodexResource,
    },
}

/// Handle for interacting with the Codex actor.
#[derive(Clone)]
pub struct CodexActorHandle {
    actor: ActorRef<CodexActorMessage>,
}

impl CodexActorHandle {
    /// Request a credential based on target model mask. Returns `None` if none available.
    pub async fn get_credential(&self, model_mask: u64) -> Result<Option<CodexLease>, PolluxError> {
        ractor::call!(self.actor, CodexActorMessage::GetCredential, model_mask)
            .map_err(|e| PolluxError::RactorError(format!("GetCredential RPC failed: {e}")))
    }

    /// Report rate limit; the actor will cool down this credential before reuse.
    pub async fn report_rate_limit(&self, id: CredentialId, model_mask: u64, cooldown: Duration) {
        let _ = ractor::cast!(
            self.actor,
            CodexActorMessage::ReportRateLimit {
                id,
                model_mask,
                cooldown
            }
        );
    }

    /// Report invalid/expired access (401); the actor will refresh before reuse.
    pub async fn report_invalid(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, CodexActorMessage::ReportInvalid { id });
    }

    /// Report that a credential does not support a model (e.g. 404).
    pub async fn report_model_unsupported(&self, id: CredentialId, model_mask: u64) {
        let _ = ractor::cast!(
            self.actor,
            CodexActorMessage::ReportModelUnsupported { id, model_mask }
        );
    }

    /// Report a credential as permanently banned/unusable; remove it entirely.
    pub async fn report_baned(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, CodexActorMessage::ReportBaned { id });
    }

    /// Submit a trusted OAuth token response to the actor for persistence + activation.
    pub(crate) async fn submit_trusted_oauth(&self, token_response: OauthTokenResponse) {
        let _ = ractor::cast!(
            self.actor,
            CodexActorMessage::SubmitTrustedOauth(token_response)
        );
    }

    /// Submit refresh tokens as 0-trust seeds. The actor will refresh, then persist+activate.
    pub(crate) async fn submit_refresh_tokens(&self, refresh_tokens: Vec<String>) {
        let seeds: Vec<CodexRefreshTokenSeed> = refresh_tokens
            .into_iter()
            .filter_map(CodexRefreshTokenSeed::new)
            .collect();

        if seeds.is_empty() {
            return;
        }

        let _ = ractor::cast!(self.actor, CodexActorMessage::SubmitUntrustedSeeds(seeds));
    }

    pub(in crate::providers::codex) fn send_refresh_complete(
        &self,
        outcome: RefreshOutcome,
    ) -> Result<(), PolluxError> {
        ractor::cast!(self.actor, CodexActorMessage::RefreshComplete { outcome })
            .map_err(|e| PolluxError::RactorError(format!("RefreshComplete cast failed: {e}")))
    }
}

struct CodexActorState {
    ops: CredentialOps,
    manager: CredentialManager,
    model_caps_all: u64,
    refresh_handle: CodexRefresherHandle,
}

struct CodexActor;

#[ractor::async_trait]
impl Actor for CodexActor {
    type Msg = CodexActorMessage;
    type State = CodexActorState;
    type Arguments = (crate::db::DbActorHandle, Arc<CodexResolvedConfig>);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (db, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let ops = CredentialOps::new(db);

        let refresh_handle = CodexRefresherHandle::spawn(
            CodexActorHandle {
                actor: myself.clone(),
            },
            cfg.clone(),
        )
        .await?;

        let model_count = MODEL_REGISTRY.len();
        let model_caps_all = *SUPPORTED_MODEL_MASK;

        let mut manager = CredentialManager::new(model_count);

        let model_names = (*SUPPORTED_MODEL_NAMES).clone();
        info!(
            "CodexActor initializing with supported models: {:?}",
            model_names
        );

        let rows = ops.load_active().await.map_err(|e| {
            ActorProcessingErr::from(format!("DB load active codex creds failed: {e}"))
        })?;
        for (id, cred) in rows {
            manager.add_credential(id, cred, model_caps_all);
        }

        info!(
            "CodexActor started from DB: {} active creds loaded into {} queues",
            manager.total_creds(),
            model_count
        );

        info!(
            proxy = %cfg.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            enable_multiplexing = cfg.enable_multiplexing,
            retry_max_times = cfg.retry_max_times,
            oauth_tps = cfg.oauth_tps,
            responses_url = %crate::providers::codex::CODEX_RESPONSES_URL.as_str(),
            "CodexActor runtime config loaded"
        );

        Ok(CodexActorState {
            ops,
            manager,
            model_caps_all,
            refresh_handle,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CodexActorMessage::GetCredential(model_mask, rp) => {
                self.handle_get_credential(myself.clone(), state, rp, model_mask)
                    .await;
            }

            CodexActorMessage::ReportRateLimit {
                id,
                model_mask,
                cooldown,
            } => {
                self.handle_report_rate_limit(state, id, model_mask, cooldown);
            }

            CodexActorMessage::ReportModelUnsupported { id, model_mask } => {
                self.handle_report_model_unsupported(state, id, model_mask);
            }

            CodexActorMessage::ReportInvalid { id } => {
                self.handle_report_invalid(myself.clone(), state, vec![id])
                    .await;
            }

            CodexActorMessage::ReportBaned { id } => {
                self.handle_report_baned(state, id).await;
            }

            CodexActorMessage::SubmitTrustedOauth(token_response) => {
                self.handle_ingest_oauth_response(myself.clone(), state, token_response, None)
                    .await;
            }

            CodexActorMessage::SubmitUntrustedSeeds(seeds) => {
                self.handle_submit_untrusted_seeds(state, seeds).await;
            }

            CodexActorMessage::RefreshComplete { outcome } => {
                self.handle_refresh_complete(myself.clone(), state, outcome)
                    .await;
            }

            CodexActorMessage::ActivateCredential { id, credential } => {
                let account_id = credential.account_id().to_string();
                state
                    .manager
                    .add_credential(id, credential, state.model_caps_all);
                info!("ID: {id}, Account: {account_id}, submitted and activated");
            }
        }
        Ok(())
    }
}

impl CodexActor {
    fn handle_report_model_unsupported(
        &self,
        state: &mut CodexActorState,
        id: CredentialId,
        model_mask: u64,
    ) {
        if model_mask == 0 || !state.manager.contains(id) {
            return;
        }

        let account_id = state
            .manager
            .account_id_of(id)
            .unwrap_or_else(|| "-".to_string());

        let disabled_names = crate::model_catalog::format_model_mask(model_mask);

        // Scheduler is pure logic; log the state transition at the actor boundary.
        let Some((before_bits, after_bits)) = state.manager.mark_model_unsupported(id, model_mask)
        else {
            return;
        };
        if before_bits == after_bits {
            return;
        }

        if after_bits == 0 {
            warn!(
                "Codex credential id={} account={} now supports no models after disabling {} (mask=0x{:016x}); caps 0x{:016x} -> 0x{:016x}",
                id, account_id, disabled_names, model_mask, before_bits, after_bits
            );
        } else {
            info!(
                "Codex credential id={} account={} disabled models {} (mask=0x{:016x}); caps 0x{:016x} -> 0x{:016x}",
                id, account_id, disabled_names, model_mask, before_bits, after_bits
            );
        }
    }

    async fn handle_get_credential(
        &self,
        myself: ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        reply_port: RpcReplyPort<Option<CodexLease>>,
        model_mask: u64,
    ) {
        let assignment = state.manager.get_assigned(model_mask);

        if !assignment.refresh_ids.is_empty() {
            self.handle_report_invalid(myself, state, assignment.refresh_ids)
                .await;
        }

        if let Some(assigned) = assignment.assigned {
            info!(
                "Get credential: ID: {}, Account: {}, model_mask=0x{:016x}, queue_len={}",
                assigned.id,
                assigned.account_id,
                model_mask,
                state.manager.queue_len(model_mask)
            );
            let _ = reply_port.send(Some(assigned));
            return;
        }

        warn!(
            "No credential available for model_mask=0x{:016x}, queue_len={}, cooldowns={}, refreshing={}",
            model_mask,
            state.manager.queue_len(model_mask),
            state.manager.cooldown_len(),
            state.manager.refreshing_len()
        );
        let _ = reply_port.send(None);
    }

    fn handle_report_rate_limit(
        &self,
        state: &mut CodexActorState,
        id: CredentialId,
        model_mask: u64,
        cooldown: Duration,
    ) {
        if !state.manager.contains(id) {
            return;
        }
        state.manager.report_rate_limit(id, model_mask, cooldown);
        info!(
            "ID: {id}, Credential starting cooldown, model_mask=0x{:016x}, re-enqueue after {} secs",
            model_mask,
            cooldown.as_secs(),
        );
    }

    async fn handle_report_invalid(
        &self,
        myself: ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        ids: Vec<CredentialId>,
    ) {
        let mut jobs_to_send = Vec::new();
        for id in ids {
            if state.manager.is_refreshing(id) {
                debug!("ID: {id} already refreshing, skipping.");
                continue;
            }
            if let Some(current) = state.manager.get_full_credential_copy(id) {
                state.manager.mark_refreshing(id);

                info!(
                    "ID: {}, Account: {}, invalid/expired reported.",
                    id,
                    current.account_id()
                );
                jobs_to_send.push((id, current));
            }
        }
        if jobs_to_send.is_empty() {
            return;
        }

        let refresh_handle = state.refresh_handle.clone();
        tokio::spawn(async move {
            for (id, cred) in jobs_to_send {
                if let Err(e) = refresh_handle.submit_refresh(id, cred.clone()) {
                    warn!("ID: {id} refresh enqueue failed. Rolling back.");
                    let _ = myself.cast(CodexActorMessage::RefreshComplete {
                        outcome: RefreshOutcome::RefreshCredential {
                            id,
                            cred,
                            result: Err(e),
                        },
                    });
                } else {
                    debug!("ID: {id} refresh enqueued.");
                }
            }
        });
    }

    async fn handle_report_baned(&self, state: &mut CodexActorState, id: CredentialId) {
        let account_id = state
            .manager
            .account_id_of(id)
            .unwrap_or_else(|| "-".to_string());
        let removed = state.manager.contains(id);

        state.manager.delete_credential(id);

        let ops = state.ops.clone();
        let account_id_for_db = account_id.clone();
        tokio::spawn(async move {
            if let Err(e) = ops.set_status(id, false).await {
                warn!(
                    "ID: {id}, Account: {account_id_for_db}, ban report failed to update DB status: {}",
                    e
                );
            }
        });

        info!(
            "ID: {id}, Account: {account_id}, banned. removed_from_mem={}",
            removed
        );
    }

    async fn handle_submit_untrusted_seeds(
        &self,
        state: &mut CodexActorState,
        seeds: Vec<CodexRefreshTokenSeed>,
    ) {
        let count = seeds.len();
        info!(count, "Batch submit received, dispatching...");
        let refresh_handle = state.refresh_handle.clone();

        tokio::spawn(async move {
            for seed in seeds {
                if let Err(e) = refresh_handle.submit_initial_refresh(seed) {
                    warn!("Failed to enqueue submit refresh: {}", e);
                    break;
                }
            }
        });
    }

    async fn handle_ingest_oauth_response(
        &self,
        myself: ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        token_response: OauthTokenResponse,
        refresh_seed: Option<CodexRefreshTokenSeed>,
    ) {
        let cred = match CodexResource::try_from_oauth_token_response(token_response, refresh_seed)
        {
            Ok(cred) => cred,
            Err(e) => {
                warn!("Codex credential submit failed: {}", e);
                return;
            }
        };

        let account_id = cred.account_id().to_string();
        let ops = state.ops.clone();

        tokio::spawn(async move {
            let cred_for_db = cred.clone();
            match ops.upsert(cred_for_db).await {
                Ok(new_id) => {
                    if let Err(e) = myself.cast(CodexActorMessage::ActivateCredential {
                        id: new_id,
                        credential: cred,
                    }) {
                        warn!("Account: {account_id} ActivateCredential failed: {}", e);
                    }
                }
                Err(e) => warn!("Account: {account_id} DB upsert failed: {}", e),
            }
        });
    }

    async fn handle_refresh_complete(
        &self,
        myself: ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        outcome: RefreshOutcome,
    ) {
        match outcome {
            RefreshOutcome::RefreshCredential { id, cred, result } => match result {
                Ok(()) => {
                    if !state.manager.is_refreshing(id) {
                        debug!("ID: {id} refresh completed after removal; skipping.");
                        return;
                    }

                    debug!("ID: {id} refresh success. Updating manager and persisting.");
                    state
                        .manager
                        .add_credential(id, cred.clone(), state.model_caps_all);

                    let ops = state.ops.clone();
                    tokio::spawn(async move {
                        let patch = CodexPatch {
                            email: cred.email().map(ToString::to_string),
                            refresh_token: Some(cred.refresh_token().to_string()),
                            access_token: Some(cred.access_token().to_string()),
                            expiry: Some(cred.expiry()),
                            chatgpt_plan_type: cred.chatgpt_plan_type().map(ToString::to_string),
                            ..Default::default()
                        };

                        if let Err(e) = ops.update_by_id(id, patch).await {
                            warn!("ID: {id} DB update failed: {}", e);
                        }
                    });
                }

                Err(err) => {
                    if !state.manager.is_refreshing(id) {
                        debug!("ID: {id} refresh failed after removal; skipping.");
                        return;
                    }

                    match err {
                        PolluxError::Oauth(OauthError::ServerResponse { .. }) => {
                            error!("ID: {id} refresh failed permanently: {}. Removing.", err);
                            state.manager.delete_credential(id);

                            let ops = state.ops.clone();
                            tokio::spawn(async move {
                                if let Err(e) = ops.set_status(id, false).await {
                                    warn!("ID: {id} DB set_status failed: {}", e);
                                }
                            });
                        }

                        _ => {
                            warn!(
                                "ID: {id} refresh failed due to transient error: {}. Keeping credential.",
                                err
                            );
                            state.manager.add_credential(id, cred, state.model_caps_all);
                        }
                    }
                }
            },

            RefreshOutcome::InitialOauthTokenResponse { seed, result } => match result {
                Ok(token_response) => {
                    self.handle_ingest_oauth_response(
                        myself.clone(),
                        state,
                        token_response,
                        Some(seed),
                    )
                    .await;
                }
                Err(err) => {
                    let context = match &err {
                        PolluxError::JsonError(_)
                        | PolluxError::Oauth(OauthError::Parse { .. }) => {
                            " (upstream token endpoint returned unexpected JSON)"
                        }
                        PolluxError::Oauth(OauthError::ServerResponse { .. }) => {
                            " (oauth2 server response error)"
                        }
                        _ => "",
                    };
                    warn!(
                        "Codex initial refresh failed; discarding seed{}. Details: {}",
                        context, err
                    );
                }
            },
        }
    }
}

pub(in crate::providers) async fn spawn(
    db: crate::db::DbActorHandle,
    cfg: Arc<CodexResolvedConfig>,
) -> CodexActorHandle {
    let (actor, _jh) = ractor::Actor::spawn(Some("CodexMain".to_string()), CodexActor, (db, cfg))
        .await
        .expect("failed to spawn CodexActor");

    CodexActorHandle { actor }
}
