use super::{ops::CredentialOps, router::RouteTable};
use crate::config::CodexResolvedConfig;
use crate::db::CodexPatch;
use crate::error::{OauthError, PolluxError};
use crate::model_catalog::MODEL_REGISTRY;
use crate::providers::codex::resource::CodexResource;
use crate::providers::codex::{
    CodexRefreshTokenSeed, SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES, oauth::OauthTokenResponse,
};
use crate::providers::manifest::CodexLease;
use crate::providers::traits::scheduler::{CredentialId, ResourceScheduler};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, warn};

use super::super::{
    CodexOauthWorkerHandle, CredentialJob, CredentialJobKind, CredentialProcessError,
    CredentialProcessResult,
};

/// Public messages handled by the Codex actor.
#[derive(Debug)]
pub enum CodexActorMessage {
    /// Request one available credential for the given model mask.
    /// The optional `u64` is the route_key (ahash of session_id) for session affinity.
    /// Returns `None` if none available.
    GetCredential {
        model_mask: u64,
        route_key: Option<u64>,
        reply: RpcReplyPort<Option<CodexLease>>,
    },

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
    /// This should already contain access_token + expiry + id_token. The actor will convert it
    /// into a trusted ingest job, then persist+activate it through the same completion path as
    /// other credential ingest flows.
    SubmitTrustedOauth(OauthTokenResponse),

    /// Submit untrusted refresh token seeds and trigger zero-trust ingestion for each.
    ///
    /// This is intended for 0-trust ingestion (e.g. an add-credentials endpoint). The actor will
    /// only persist+activate after a refresh succeeds and identity can be derived.
    SubmitUntrustedSeeds(Vec<CodexRefreshTokenSeed>),

    // Internal messages (sent by the actor itself / workers)
    /// Background credential processing has completed.
    ProcessComplete { result: CredentialProcessResult },
    /// A credential has been processed and stored; activate it in memory queues.
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
    /// Request a credential based on target model mask.
    /// If `route_key` is provided, the actor will attempt session-affinity routing first.
    pub async fn get_credential(
        &self,
        model_mask: u64,
        route_key: Option<u64>,
    ) -> Result<Option<CodexLease>, PolluxError> {
        ractor::call!(self.actor, |reply| CodexActorMessage::GetCredential {
            model_mask,
            route_key,
            reply,
        })
        .map_err(|e| PolluxError::RactorError(format!("GetCredential RPC failed: {e}")))
    }

    /// Report rate limit; the actor will cool down this credential before reuse.
    pub fn report_rate_limit(&self, id: CredentialId, model_mask: u64, cooldown: Duration) {
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
    pub fn report_invalid(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, CodexActorMessage::ReportInvalid { id });
    }

    /// Report that a credential does not support a model (e.g. 404).
    pub fn report_model_unsupported(&self, id: CredentialId, model_mask: u64) {
        let _ = ractor::cast!(
            self.actor,
            CodexActorMessage::ReportModelUnsupported { id, model_mask }
        );
    }

    /// Report a credential as permanently banned/unusable; remove it entirely.
    pub fn report_baned(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, CodexActorMessage::ReportBaned { id });
    }

    /// Submit a trusted OAuth token response to the actor for trusted ingest + persistence.
    pub(crate) fn submit_trusted_oauth(&self, token_response: OauthTokenResponse) {
        let _ = ractor::cast!(
            self.actor,
            CodexActorMessage::SubmitTrustedOauth(token_response)
        );
    }

    /// Submit refresh tokens as 0-trust seeds. The actor will verify, then persist+activate.
    pub(crate) fn submit_refresh_tokens(&self, refresh_tokens: Vec<String>) {
        let seeds: Vec<CodexRefreshTokenSeed> = refresh_tokens
            .into_iter()
            .filter_map(CodexRefreshTokenSeed::new)
            .collect();

        if seeds.is_empty() {
            return;
        }

        let _ = ractor::cast!(self.actor, CodexActorMessage::SubmitUntrustedSeeds(seeds));
    }

    pub(in crate::providers::codex) fn send_process_complete(
        &self,
        result: CredentialProcessResult,
    ) -> Result<(), PolluxError> {
        ractor::cast!(self.actor, CodexActorMessage::ProcessComplete { result })
            .map_err(|e| PolluxError::RactorError(format!("ProcessComplete cast failed: {e}")))
    }
}

struct CodexActorState {
    ops: CredentialOps,
    manager: ResourceScheduler<CodexResource>,
    router: RouteTable,
    provider_supported_mask: u64,
    processor_handle: CodexOauthWorkerHandle,
}

struct CodexActor;

#[ractor::async_trait]
impl Actor for CodexActor {
    type Msg = CodexActorMessage;
    type State = CodexActorState;
    type Arguments = (CredentialOps, Arc<CodexResolvedConfig>);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (ops, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let processor_handle = CodexOauthWorkerHandle::spawn(
            CodexActorHandle {
                actor: myself.clone(),
            },
            cfg.clone(),
        )
        .await?;

        let model_count = MODEL_REGISTRY.len();
        let provider_supported_mask = *SUPPORTED_MODEL_MASK;

        let mut manager = ResourceScheduler::new(model_count);

        let model_names = (*SUPPORTED_MODEL_NAMES).clone();
        info!(
            "CodexActor initializing with supported models: {:?}",
            model_names
        );

        let rows = ops.load_active().await.map_err(|e| {
            ActorProcessingErr::from(format!("DB load active codex creds failed: {e}"))
        })?;
        for (id, cred) in rows {
            manager.add_credential(id, cred, provider_supported_mask);
        }

        info!(
            "CodexActor started from DB: {} active creds loaded into {} queues",
            manager.stats(0).total_creds,
            model_count
        );

        info!(
            custom_api_url = %cfg.custom_api_url,
            proxy = %cfg.proxy.as_ref().map_or("<none>", |u| u.as_str()),
            enable_multiplexing = cfg.enable_multiplexing,
            retry_max_times = cfg.retry_max_times,
            oauth_tps = cfg.oauth_tps,
            "CodexActor runtime config loaded"
        );

        let router = RouteTable::new(10_000, std::time::Duration::from_secs(3600));

        Ok(CodexActorState {
            ops,
            manager,
            router,
            provider_supported_mask,
            processor_handle,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CodexActorMessage::GetCredential {
                model_mask,
                route_key,
                reply,
            } => {
                Self::handle_get_credential(myself.clone(), state, reply, model_mask, route_key);
            }

            CodexActorMessage::ReportRateLimit {
                id,
                model_mask,
                cooldown,
            } => {
                Self::handle_report_rate_limit(state, id, model_mask, cooldown);
            }

            CodexActorMessage::ReportModelUnsupported { id, model_mask } => {
                Self::handle_report_model_unsupported(state, id, model_mask);
            }

            CodexActorMessage::ReportInvalid { id } => {
                Self::handle_report_invalid(myself.clone(), state, vec![id]);
            }

            CodexActorMessage::ReportBaned { id } => {
                Self::handle_report_baned(state, id);
            }

            CodexActorMessage::SubmitTrustedOauth(token_response) => {
                Self::handle_submit_trusted_oauth(state, token_response);
            }

            CodexActorMessage::SubmitUntrustedSeeds(seeds) => {
                Self::handle_submit_untrusted_seeds(state, seeds);
            }

            CodexActorMessage::ProcessComplete { result } => {
                Self::handle_process_complete(&myself, state, result);
            }

            CodexActorMessage::ActivateCredential { id, credential } => {
                let account_id = credential.account_id().to_string();
                state
                    .manager
                    .add_credential(id, credential, state.provider_supported_mask);
                info!("ID: {id}, Account: {account_id}, submitted and activated");
            }
        }
        Ok(())
    }
}

impl CodexActor {
    fn handle_report_model_unsupported(
        state: &mut CodexActorState,
        id: CredentialId,
        model_mask: u64,
    ) {
        if model_mask == 0 || !state.manager.contains(id) {
            return;
        }

        let account_id = state
            .manager
            .get_credential(id)
            .map_or_else(|| "-".to_string(), |r| r.account_id().to_string());

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

    fn handle_get_credential(
        myself: ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        reply_port: RpcReplyPort<Option<CodexLease>>,
        model_mask: u64,
        route_key: Option<u64>,
    ) {
        let sticky_id = route_key.and_then(|rk| state.router.get(rk, model_mask));
        let start = std::time::Instant::now();
        let assignment = state.manager.get_assigned(model_mask, sticky_id);
        let sched_us = start.elapsed().as_micros();

        if !assignment.refresh_ids.is_empty() {
            Self::handle_report_invalid(myself, state, assignment.refresh_ids);
        }

        let stats = state.manager.stats(model_mask);

        if let Some(assigned) = assignment.assigned {
            if let Some(rk) = route_key
                && !assignment.route_hit
            {
                state.router.insert(rk, model_mask, assigned.id);
            }

            info!(
                sched_us,
                id = assigned.id,
                account = %assigned.account_id,
                email = %assigned.email.as_deref().unwrap_or("-"),
                model_mask = format_args!("0x{model_mask:016x}"),
                sticky = assignment.route_hit,
                queue = stats.queue_len,
                total = stats.total_creds,
                cooling = stats.cooldowns,
                refreshing = stats.refreshing,
                "[Codex] Credential assigned"
            );
            let _ = reply_port.send(Some(assigned));
            return;
        }

        warn!(
            model_mask = format_args!("0x{model_mask:016x}"),
            sticky_id = ?sticky_id,
            queue = stats.queue_len,
            total = stats.total_creds,
            cooling = stats.cooldowns,
            refreshing = stats.refreshing,
            skipped.cooling = assignment.stats.skipped_cooling,
            skipped.refreshing = assignment.stats.skipped_refreshing,
            skipped.expired = assignment.stats.skipped_expired,
            "[Codex] No credential available"
        );
        let _ = reply_port.send(None);
    }

    fn handle_report_rate_limit(
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

    fn handle_report_invalid(
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
            if let Some(current) = state.manager.get_credential_clone(id) {
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

        let processor_handle = state.processor_handle.clone();
        tokio::spawn(async move {
            for (id, cred) in jobs_to_send {
                let job = CredentialJob::refresh(id, cred);
                if let Err(e) = processor_handle.submit(job.clone()) {
                    warn!("ID: {id} credential refresh enqueue failed. Rolling back.");
                    let _ = myself.cast(CodexActorMessage::ProcessComplete {
                        result: Err(CredentialProcessError {
                            original_job: job,
                            error: e,
                        }),
                    });
                } else {
                    debug!("ID: {id} refresh enqueued.");
                }
            }
        });
    }

    fn handle_report_baned(state: &mut CodexActorState, id: CredentialId) {
        let account_id = state
            .manager
            .get_credential(id)
            .map_or_else(|| "-".to_string(), |r| r.account_id().to_string());
        let removed = state.manager.contains(id);

        state.manager.delete_credential(id);

        info!("ID: {id}, Account: {account_id}, banned. removed_from_mem={removed}");

        let ops = state.ops.clone();
        tokio::spawn(async move {
            if let Err(e) = ops.set_status(id, false).await {
                warn!(
                    "ID: {id}, Account: {account_id}, ban report failed to update DB status: {e}"
                );
            }
        });
    }

    fn handle_submit_untrusted_seeds(
        state: &mut CodexActorState,
        seeds: Vec<CodexRefreshTokenSeed>,
    ) {
        let count = seeds.len();
        info!(count, "Batch submit received, dispatching...");
        let processor_handle = state.processor_handle.clone();

        tokio::spawn(async move {
            for seed in seeds {
                let job = match CredentialJob::ingest_untrusted_seed(seed) {
                    Ok(job) => job,
                    Err(e) => {
                        warn!(
                            "Failed to build untrusted Codex ingest job from seed: {}",
                            e
                        );
                        continue;
                    }
                };

                if let Err(e) = processor_handle.submit(job) {
                    warn!("Failed to enqueue untrusted Codex ingest job: {}", e);
                    break;
                }
            }
        });
    }

    fn handle_submit_trusted_oauth(
        state: &mut CodexActorState,
        token_response: OauthTokenResponse,
    ) {
        info!("Trusted OAuth submit received, dispatching trusted ingest...");
        let processor_handle = state.processor_handle.clone();
        tokio::spawn(async move {
            let job = match CredentialJob::ingest_trusted_oauth(token_response) {
                Ok(job) => job,
                Err(e) => {
                    warn!("Trusted OAuth submit ignored: {}", e);
                    return;
                }
            };

            if let Err(e) = processor_handle.submit(job) {
                warn!("Trusted OAuth submit enqueue failed: {}", e);
            }
        });
    }

    fn handle_process_complete(
        myself: &ActorRef<CodexActorMessage>,
        state: &mut CodexActorState,
        result: CredentialProcessResult,
    ) {
        let kind = match &result {
            Ok(success) => &success.kind,
            Err(failed) => &failed.original_job.kind,
        };
        if let Some(id) = kind.credential_id()
            && !state.manager.is_refreshing(id)
        {
            debug!("ID: {id} credential processing completed/failed after removal; skipping.");
            return;
        }

        match result {
            Ok(success) => {
                let account_id = success.cred.account_id().to_string();
                let cred = success.cred;
                match success.kind {
                    CredentialJobKind::Refresh(id) => {
                        debug!("ID: {id} refresh success. Updating manager and persisting.");
                        state.manager.complete_refresh(id, cred.clone());

                        let ops = state.ops.clone();
                        tokio::spawn(async move {
                            let patch = CodexPatch {
                                email: cred.email().map(ToString::to_string),
                                refresh_token: Some(cred.refresh_token().to_string()),
                                access_token: Some(cred.access_token().to_string()),
                                expiry: Some(cred.expiry()),
                                chatgpt_plan_type: cred
                                    .chatgpt_plan_type()
                                    .map(ToString::to_string),
                                ..Default::default()
                            };

                            if let Err(e) = ops.update_by_id(id, patch).await {
                                warn!("ID: {id} DB update failed: {}", e);
                            }
                        });
                    }
                    CredentialJobKind::IngestUntrusted | CredentialJobKind::IngestTrusted => {
                        info!("Account: {account_id} Codex ingest success. Inserting to DB.");
                        let ops = state.ops.clone();
                        let myself = myself.clone();
                        tokio::spawn(async move {
                            let cred_for_db = cred.clone();
                            match ops.upsert(cred_for_db).await {
                                Ok(new_id) => {
                                    if let Err(e) =
                                        myself.cast(CodexActorMessage::ActivateCredential {
                                            id: new_id,
                                            credential: cred,
                                        })
                                    {
                                        warn!(
                                            "Account: {account_id} ActivateCredential failed: {}",
                                            e
                                        );
                                    }
                                }
                                Err(e) => warn!("Account: {account_id} DB upsert failed: {}", e),
                            }
                        });
                    }
                }
            }
            Err(failed) => {
                let job = failed.original_job;
                let err = failed.error;
                let account = job.cred.account_id().to_string();
                warn!("CredentialJob failed for account {}: {}", account, err);

                match job.kind {
                    CredentialJobKind::Refresh(id) => {
                        if let PolluxError::Oauth(OauthError::ServerResponse { .. }) = err {
                            error!("ID: {id} refresh failed permanently: {}. Removing.", err);
                            state.manager.delete_credential(id);

                            let ops = state.ops.clone();
                            tokio::spawn(async move {
                                if let Err(e) = ops.set_status(id, false).await {
                                    warn!("ID: {id} DB set_status failed: {}", e);
                                }
                            });
                        } else {
                            warn!(
                                "ID: {id} refresh failed due to transient error: {}. Keeping credential.",
                                err
                            );
                            state.manager.complete_refresh(id, job.cred);
                        }
                    }
                    CredentialJobKind::IngestUntrusted => {
                        warn!(
                            "Untrusted Codex credential ingest failed; discarding job. Details: {}",
                            err
                        );
                    }
                    CredentialJobKind::IngestTrusted => {
                        warn!(
                            "Trusted Codex OAuth ingest failed; discarding job. Details: {}",
                            err
                        );
                    }
                }
            }
        }
    }
}

pub(in crate::providers) async fn spawn(
    db: crate::db::DbActorHandle,
    cfg: Arc<CodexResolvedConfig>,
) -> CodexActorHandle {
    let ops = CredentialOps::new(db);

    let (actor, _jh) = ractor::Actor::spawn(Some("CodexMain".to_string()), CodexActor, (ops, cfg))
        .await
        .expect("failed to spawn CodexActor");

    CodexActorHandle { actor }
}
