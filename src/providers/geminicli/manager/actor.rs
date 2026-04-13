use super::ops::CredentialOps;
use crate::config::GeminiCliResolvedConfig;
use crate::db::GeminiCliPatch;
use crate::error::{OauthError, PolluxError};
use crate::model_catalog::MODEL_REGISTRY;
use crate::providers::geminicli::client::oauth::endpoints::GoogleTokenResponse;
use crate::providers::geminicli::client::oauth::utils::attach_email_from_id_token;
use crate::providers::geminicli::resource::GeminiCliResource;
use crate::providers::geminicli::workers::{
    CredentialJob, CredentialJobKind, CredentialProcessError, CredentialProcessResult,
    GeminiCliOauthWorkerHandle,
};
use crate::providers::geminicli::{SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES};
use crate::providers::manifest::{GeminiCliLease, GeminiCliProfile};
use crate::providers::traits::scheduler::{CredentialId, ResourceScheduler};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub(crate) struct GeminiCliRefreshTokenSeed {
    refresh_token: String,
}

impl GeminiCliRefreshTokenSeed {
    pub fn new(refresh_token: &str) -> Option<Self> {
        let refresh_token = refresh_token.trim().to_string();
        if refresh_token.is_empty() {
            return None;
        }
        Some(Self { refresh_token })
    }
}

/// Public messages handled by the Gemini CLI actor.
pub enum GeminiCliActorMessage {
    /// Request one available credential for the given model mask. Err if none available.
    GetCredential(u64, RpcReplyPort<Option<GeminiCliLease>>),
    /// Report rate limiting for a model mask; start cooldown with lazy re-enqueue.
    ReportRateLimit {
        id: CredentialId,
        cooldown: Duration,
        model_mask: u64,
    },
    /// Report unsupported model (e.g. 400/404); clear capability bits for this credential.
    ReportModelUnsupported { id: CredentialId, model_mask: u64 },
    /// Report invalid/expired access (e.g. 401/403); refresh then re-enqueue.
    ReportInvalid { id: CredentialId },
    /// Report a credential as banned/unusable; remove from queues and storage.
    ReportBaned { id: CredentialId },

    /// Submit a batch of credentials and trigger one refresh pass for each.
    SubmitCredentials(Vec<GeminiCliProfile>),
    /// Submit a trusted OAuth token response to the actor for onboarding + persistence.
    SubmitTrustedOauth(GoogleTokenResponse),
    /// Submit refresh tokens as 0-trust seeds. The actor will refresh, onboard, then persist+activate.
    SubmitUntrustedSeeds(Vec<GeminiCliRefreshTokenSeed>),

    // Internal messages (sent by the actor itself)
    /// Token refresh has completed; update stored credential and re-enqueue if ok.
    ProcessComplete { result: CredentialProcessResult },
    /// A credential has been refreshed and stored; activate it in memory queues.
    ActivateCredential {
        id: CredentialId,
        credential: GeminiCliResource,
    },
}

/// Handle for interacting with the Gemini CLI actor.
#[derive(Clone)]
pub struct GeminiCliActorHandle {
    actor: ActorRef<GeminiCliActorMessage>,
}

impl GeminiCliActorHandle {
    /// Request a credential based on target model mask. Returns error if none available.
    pub async fn get_credential(
        &self,
        model_mask: u64,
    ) -> Result<Option<GeminiCliLease>, PolluxError> {
        ractor::call!(self.actor, GeminiCliActorMessage::GetCredential, model_mask)
            .map_err(|e| PolluxError::RactorError(format!("GetCredential RPC failed:: {e}")))
    }

    /// Report rate limit; the actor will cool down this credential before reuse.
    pub fn report_rate_limit(&self, id: CredentialId, model_mask: u64, cooldown: Duration) {
        let _ = ractor::cast!(
            self.actor,
            GeminiCliActorMessage::ReportRateLimit {
                id,
                cooldown,
                model_mask
            }
        );
    }

    /// Report invalid/expired (401/403); the actor will refresh before reuse.
    pub fn report_invalid(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, GeminiCliActorMessage::ReportInvalid { id });
    }

    /// Report that a credential does not support a model (e.g. 400/404).
    pub fn report_model_unsupported(&self, id: CredentialId, model_mask: u64) {
        let _ = ractor::cast!(
            self.actor,
            GeminiCliActorMessage::ReportModelUnsupported { id, model_mask }
        );
    }

    /// Report a credential as permanently banned/unusable; remove it entirely.
    pub fn report_baned(&self, id: CredentialId) {
        let _ = ractor::cast!(self.actor, GeminiCliActorMessage::ReportBaned { id });
    }

    /// Submit new credentials to the actor and trigger refresh for each.
    pub fn submit_credentials(&self, creds: Vec<GeminiCliProfile>) {
        let _ = ractor::cast!(self.actor, GeminiCliActorMessage::SubmitCredentials(creds));
    }

    /// Submit a trusted OAuth token response to the actor for persistence + activation.
    pub(crate) fn submit_trusted_oauth(&self, token_response: GoogleTokenResponse) {
        let _ = ractor::cast!(
            self.actor,
            GeminiCliActorMessage::SubmitTrustedOauth(token_response)
        );
    }

    /// Submit refresh tokens as 0-trust seeds. The actor will refresh, onboard, then persist+activate.
    pub(crate) fn submit_refresh_tokens(&self, refresh_tokens: Vec<String>) {
        let seeds: Vec<GeminiCliRefreshTokenSeed> = refresh_tokens
            .into_iter()
            .filter_map(|t| GeminiCliRefreshTokenSeed::new(&t))
            .collect();

        if seeds.is_empty() {
            return;
        }

        let _ = ractor::cast!(
            self.actor,
            GeminiCliActorMessage::SubmitUntrustedSeeds(seeds)
        );
    }

    pub(in crate::providers::geminicli) fn send_process_complete(
        &self,
        result: CredentialProcessResult,
    ) -> Result<(), PolluxError> {
        ractor::cast!(
            self.actor,
            GeminiCliActorMessage::ProcessComplete { result }
        )
        .map_err(|e| PolluxError::RactorError(format!("ProcessComplete cast failed: {e}")))
    }
}

/// Internal state held by ractor-driven Gemini CLI actor.
struct GeminiCliActorState {
    ops: CredentialOps,
    manager: ResourceScheduler<GeminiCliResource>,
    provider_supported_mask: u64,
    processor_handle: GeminiCliOauthWorkerHandle,
}

/// ractor-based Gemini CLI actor.
struct GeminiCliActor;

#[ractor::async_trait]
impl Actor for GeminiCliActor {
    type Msg = GeminiCliActorMessage;
    type State = GeminiCliActorState;
    type Arguments = (CredentialOps, Arc<GeminiCliResolvedConfig>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let (ops, cfg) = args;
        let processor_handle = GeminiCliOauthWorkerHandle::spawn(
            GeminiCliActorHandle {
                actor: _myself.clone(),
            },
            cfg.clone(),
        )
        .await?;

        let model_count = MODEL_REGISTRY.len();
        let provider_supported_mask = *SUPPORTED_MODEL_MASK;

        let mut manager = ResourceScheduler::new(model_count);

        let model_names = (*SUPPORTED_MODEL_NAMES).clone();
        info!(
            "GeminiCliActor initializing with supported models: {:?}",
            model_names
        );

        let rows = ops
            .load_active()
            .await
            .map_err(|e| ActorProcessingErr::from(format!("DB load active creds failed: {e}")))?;

        for (id, cred) in rows {
            manager.add_credential(id, cred, provider_supported_mask);
        }

        info!(
            "GeminiCliActor started from DB: {} active creds loaded into {} queues",
            manager.total_creds(),
            model_count
        );

        info!(
            custom_api_url = %cfg.custom_api_url,
            proxy = %cfg.proxy.as_ref().map_or("<none>", |u| u.as_str()),
            enable_multiplexing = cfg.enable_multiplexing,
            oauth_tps = cfg.oauth_tps,
            "GeminiCliActor runtime config loaded"
        );

        Ok(GeminiCliActorState {
            ops,
            manager,
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
            GeminiCliActorMessage::GetCredential(model_mask, rp) => {
                Self::handle_get_credential(&myself, state, rp, model_mask);
            }

            GeminiCliActorMessage::ReportRateLimit {
                id,
                cooldown,
                model_mask,
            } => {
                Self::handle_report_rate_limit(state, id, cooldown, model_mask);
            }
            GeminiCliActorMessage::ReportModelUnsupported { id, model_mask } => {
                Self::handle_report_model_unsupported(state, id, model_mask);
            }

            GeminiCliActorMessage::ReportInvalid { id } => {
                Self::handle_report_invalid(&myself, state, vec![id]);
            }
            GeminiCliActorMessage::ReportBaned { id } => {
                Self::handle_report_baned(state, id);
            }
            GeminiCliActorMessage::SubmitCredentials(creds_vec) => {
                Self::handle_submit_credentials(state, creds_vec);
            }
            GeminiCliActorMessage::SubmitTrustedOauth(token_response) => {
                Self::handle_submit_trusted_oauth(state, token_response);
            }
            GeminiCliActorMessage::SubmitUntrustedSeeds(seeds) => {
                Self::handle_submit_untrusted_seeds(state, seeds);
            }
            GeminiCliActorMessage::ProcessComplete { result } => {
                Self::handle_process_complete(&myself, state, result);
            }
            GeminiCliActorMessage::ActivateCredential { id, credential } => {
                let project = credential.project_id().to_string();
                state
                    .manager
                    .add_credential(id, credential, state.provider_supported_mask);
                info!("ID: {id}, Project: {project}, submitted and activated");
            }
        }
        Ok(())
    }
}

impl GeminiCliActor {
    fn handle_report_model_unsupported(
        state: &mut GeminiCliActorState,
        id: CredentialId,
        model_mask: u64,
    ) {
        if model_mask == 0 || !state.manager.contains(id) {
            return;
        }

        let project_id = state
            .manager
            .get_credential(id)
            .map_or_else(|| "-".to_string(), |r| r.project_id().to_string());

        // Scheduler is pure logic; log the state transition at the actor boundary.
        let Some((before_bits, after_bits)) = state.manager.mark_model_unsupported(id, model_mask)
        else {
            return;
        };
        if before_bits == after_bits {
            return;
        }

        let disabled_names = crate::model_catalog::format_model_mask(model_mask);
        if after_bits == 0 {
            warn!(
                "GeminiCli credential id={} project={} now supports no models after disabling {} (mask=0x{:016x}); caps 0x{:016x} -> 0x{:016x}",
                id, project_id, disabled_names, model_mask, before_bits, after_bits
            );
        } else {
            info!(
                "GeminiCli credential id={} project={} disabled models {} (mask=0x{:016x}); caps 0x{:016x} -> 0x{:016x}",
                id, project_id, disabled_names, model_mask, before_bits, after_bits
            );
        }
    }

    fn handle_get_credential(
        myself: &ActorRef<GeminiCliActorMessage>,
        state: &mut GeminiCliActorState,
        reply_port: RpcReplyPort<Option<GeminiCliLease>>,
        model_mask: u64,
    ) {
        let start = std::time::Instant::now();
        let assignment = state.manager.get_assigned(model_mask, None);
        let sched_us = start.elapsed().as_micros();
        let sched_stats = &assignment.stats;

        if !assignment.refresh_ids.is_empty() {
            Self::handle_report_invalid(myself, state, assignment.refresh_ids);
        }

        if let Some(assigned) = assignment.assigned {
            info!(
                sched_us,
                id = assigned.id,
                project = %assigned.project_id,
                email = %assigned.email.as_deref().unwrap_or("-"),
                model_mask = format_args!("0x{:016x}", model_mask),
                queue = sched_stats.queue_len,
                total = sched_stats.total_creds,
                cooling = sched_stats.cooldowns,
                refreshing = sched_stats.refreshing,
                "[GeminiCli] Credential assigned"
            );
            let _ = reply_port.send(Some(assigned));
            return;
        }

        warn!(
            model_mask = format_args!("0x{:016x}", model_mask),
            queue = sched_stats.queue_len,
            total = sched_stats.total_creds,
            cooling = sched_stats.cooldowns,
            refreshing = sched_stats.refreshing,
            skipped.cooling = sched_stats.skipped_cooling,
            skipped.refreshing = sched_stats.skipped_refreshing,
            skipped.expired = sched_stats.skipped_expired,
            "No credential available"
        );
        let _ = reply_port.send(None);
    }

    fn handle_report_rate_limit(
        state: &mut GeminiCliActorState,
        id: CredentialId,
        cooldown: Duration,
        model_mask: u64,
    ) {
        if !state.manager.contains(id) {
            return;
        }
        state.manager.report_rate_limit(id, model_mask, cooldown);

        info!(
            "ID: {id}, Credential starting cooldown for model_mask=0x{:016x}, lazy re-enqueue after {} secs",
            model_mask,
            cooldown.as_secs(),
        );
    }

    // handle_report_invalid, handle_report_baned, handle_submit_credentials
    fn handle_report_invalid(
        myself: &ActorRef<GeminiCliActorMessage>,
        state: &mut GeminiCliActorState,
        ids: Vec<CredentialId>,
    ) {
        let mut jobs_to_send = Vec::new();
        for id in ids {
            if state.manager.is_refreshing(id) {
                debug!("ID: {id} in batch already refreshing, skipping.");
                continue;
            }
            if let Some(current) = state.manager.get_credential_clone(id) {
                state.manager.mark_refreshing(id);

                info!(
                    "ID: {}, Project: {}, batch invalid reported.",
                    id,
                    current.project_id()
                );

                jobs_to_send.push((id, current));
            }
        }
        if jobs_to_send.is_empty() {
            return;
        }
        let processor_handle = state.processor_handle.clone();

        for (id, cred) in jobs_to_send {
            let task = CredentialJob {
                cred,
                kind: CredentialJobKind::Refresh(id),
            };
            if let Err(e) = processor_handle.submit(task.clone()) {
                warn!("ID: {id} Batch refresh enqueue failed. Rolling back.");

                let _ = myself.cast(GeminiCliActorMessage::ProcessComplete {
                    result: Err(CredentialProcessError {
                        original_job: task,
                        error: PolluxError::RactorError(format!(
                            "Failed to enqueue refresh job: {e}"
                        )),
                    }),
                });
            } else {
                debug!("ID: {id} Batch refresh enqueued.");
            }
        }
    }

    fn handle_report_baned(state: &mut GeminiCliActorState, id: CredentialId) {
        let project = state
            .manager
            .get_credential(id)
            .map_or_else(|| "-".to_string(), |r| r.project_id().to_string());
        let removed_cred = state.manager.contains(id);

        state.manager.delete_credential(id);

        let ops = state.ops.clone();
        let project_for_db = project.clone();
        tokio::spawn(async move {
            if let Err(e) = ops.set_status(id, false).await {
                warn!(
                    "ID: {id}, Project: {project_for_db}, ban report failed to update DB status: {}",
                    e
                );
            }
        });
        info!(
            "ID: {id}, Project: {project}, banned. removed_from_mem={}",
            removed_cred
        );
    }

    fn handle_submit_credentials(
        state: &mut GeminiCliActorState,
        creds_vec: Vec<GeminiCliProfile>,
    ) {
        let count = creds_vec.len();
        info!(count, "Batch submit received, dispatching...");
        let processor_handle = state.processor_handle.clone();
        tokio::spawn(async move {
            for profile in creds_vec {
                let pid = profile.project_id.clone();
                let cred = GeminiCliResource::from(profile);
                let job = CredentialJob {
                    cred,
                    kind: CredentialJobKind::Ingest,
                };
                if let Err(e) = processor_handle.submit(job) {
                    warn!(
                        "Project: {pid}, failed to enqueue onboarding refresh: {}",
                        e
                    );
                    break;
                }
            }
        });
    }

    fn handle_submit_trusted_oauth(
        state: &mut GeminiCliActorState,
        token_response: GoogleTokenResponse,
    ) {
        info!("Trusted OAuth submit received, dispatching onboarding...");
        let processor_handle = state.processor_handle.clone();
        tokio::spawn(async move {
            let mut token_value = match serde_json::to_value(&token_response) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Trusted OAuth submit ignored: token JSON encode failed: {e}");
                    return;
                }
            };
            attach_email_from_id_token(&mut token_value);

            let mut cred = GeminiCliResource::default();
            if let Err(e) = cred.update_credential(token_value) {
                warn!("Trusted OAuth submit ignored: token JSON error: {e}");
                return;
            }
            let job = CredentialJob {
                cred,
                kind: CredentialJobKind::Ingest,
            };
            if let Err(e) = processor_handle.submit(job) {
                warn!("Trusted OAuth submit enqueue failed: {}", e);
            }
        });
    }

    fn handle_submit_untrusted_seeds(
        state: &mut GeminiCliActorState,
        seeds: Vec<GeminiCliRefreshTokenSeed>,
    ) {
        let count = seeds.len();
        info!(
            count,
            "0-trust seed submit received, dispatching onboarding..."
        );
        let processor_handle = state.processor_handle.clone();
        tokio::spawn(async move {
            for seed in seeds {
                let mut cred = GeminiCliResource::default();
                if let Err(e) =
                    cred.update_credential(json!({ "refresh_token": seed.refresh_token }))
                {
                    warn!("0-trust seed discarded: JSON error: {e}");
                    continue;
                }

                let job = CredentialJob {
                    cred,
                    kind: CredentialJobKind::Ingest,
                };
                if let Err(e) = processor_handle.submit(job) {
                    warn!("0-trust seed enqueue failed: {}", e);
                    break;
                }
            }
        });
    }

    fn handle_process_complete(
        myself: &ActorRef<GeminiCliActorMessage>,
        state: &mut GeminiCliActorState,
        result: CredentialProcessResult,
    ) {
        // If the result is for a refresh task, check if the credential is still in refreshing state.
        if let Some(id) = match &result {
            Ok(success) => &success.kind,
            Err(failed) => &failed.original_job.kind,
        }
        .credential_id()
            && !state.manager.is_refreshing(id)
        {
            debug!("ID: {id} Refresh completed/failed after removal; skipping.");
            return;
        }

        // Process the refresh result: if success, update credential and re-enqueue; if failure, decide based on error type.
        match result {
            Ok(success) => {
                let pid = success.cred.project_id().to_string();
                let cred = success.cred;
                match success.kind {
                    CredentialJobKind::Refresh(id) => {
                        state.manager.complete_refresh(id, cred.clone());
                        let ops = state.ops.clone();
                        tokio::spawn(async move {
                            let patch = GeminiCliPatch {
                                email: cred.email().map(ToString::to_string),
                                access_token: Some(cred.access_token().to_string()),
                                expiry: Some(cred.expiry()),
                                ..Default::default()
                            };
                            if let Err(e) = ops.update_by_id(id, patch).await {
                                warn!("ID: {id} DB update failed: {}", e);
                            }
                        });
                    }
                    CredentialJobKind::Ingest => {
                        info!("Project: {pid} Onboard success. Inserting to DB.");
                        let ops = state.ops.clone();
                        let myself = myself.clone();
                        tokio::spawn(async move {
                            let cred_for_db = cred.clone();
                            match ops.upsert(cred_for_db).await {
                                Ok(new_id) => {
                                    if let Err(e) =
                                        myself.cast(GeminiCliActorMessage::ActivateCredential {
                                            id: new_id,
                                            credential: cred,
                                        })
                                    {
                                        warn!("Project: {pid} ActivateCredential failed: {}", e);
                                    }
                                }
                                Err(e) => warn!("Project: {pid} DB upsert failed: {}", e),
                            }
                        });
                    }
                }
            }
            Err(failed) => {
                let job = failed.original_job;
                let err = failed.error;
                let pid = job.cred.project_id().to_string();
                warn!("RefreshTask failed for project {}: {}", pid, err);
                match job.kind {
                    CredentialJobKind::Refresh(id) => {
                        if let PolluxError::Oauth(OauthError::ServerResponse { .. }) = err {
                            error!("ID: {id} Refresh failed: {}. Removing.", err);

                            state.manager.delete_credential(id);
                            let ops = state.ops.clone();
                            tokio::spawn(async move {
                                if let Err(e) = ops.set_status(id, false).await {
                                    warn!("ID: {id} DB set_status failed: {}", e);
                                }
                            });
                        } else {
                            warn!(
                                "ID: {id} Refresh failed due to transient error: {}. Keeping credential.",
                                err
                            );
                            state.manager.complete_refresh(id, job.cred);
                        }
                    }
                    CredentialJobKind::Ingest => {
                        warn!(
                            "Project: {} Onboard failed: {}. Discarding.",
                            job.cred.project_id(),
                            err
                        );
                    }
                }
            }
        }
    }
}

/// Async spawn of the Gemini CLI actor and return a handle.
pub(in crate::providers) async fn spawn(
    db: crate::db::DbActorHandle,
    gemini_cfg: Arc<GeminiCliResolvedConfig>,
) -> GeminiCliActorHandle {
    let ops = CredentialOps::new(db);

    let (actor, _jh) = Actor::spawn(
        Some("GeminiCliMain".to_string()),
        GeminiCliActor,
        (ops, gemini_cfg),
    )
    .await
    .expect("failed to spawn GeminiCliActor");
    GeminiCliActorHandle { actor }
}
