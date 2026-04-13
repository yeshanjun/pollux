use crate::config::CodexResolvedConfig;
use crate::error::{IsRetryable, OauthError, PolluxError};
use crate::providers::codex::{
    CodexRefreshTokenSeed,
    client::oauth::{OAUTH_RETRY_POLICY, endpoints::CodexOauthEndpoints},
    manager::{CodexActorHandle, CredentialId},
    oauth::OauthTokenResponse,
    resource::CodexResource,
};
use backon::{ExponentialBuilder, Retryable};
use futures::stream::StreamExt;
use governor::{Quota, RateLimiter, state::StreamRateLimitExt};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::header::{CONNECTION, HeaderMap, HeaderValue};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

#[derive(Clone, Copy, Debug)]
pub enum CredentialJobKind {
    Refresh(CredentialId),
    IngestUntrusted,
    IngestTrusted,
}

impl CredentialJobKind {
    pub fn credential_id(&self) -> Option<CredentialId> {
        match self {
            Self::Refresh(id) => Some(*id),
            Self::IngestUntrusted | Self::IngestTrusted => None,
        }
    }
}

#[derive(Debug)]
pub struct CredentialProcessError {
    pub original_job: CredentialJob,
    pub error: PolluxError,
}

pub type CredentialProcessResult = Result<CredentialJob, CredentialProcessError>;

/// Handle for submitting Codex credential processing jobs to the background actor.
#[derive(Clone)]
pub(in crate::providers::codex) struct CodexOauthWorkerHandle {
    actor: ActorRef<CodexOauthWorkerMessage>,
}

impl CodexOauthWorkerHandle {
    pub async fn spawn(
        handle: CodexActorHandle,
        cfg: Arc<CodexResolvedConfig>,
    ) -> Result<Self, ActorProcessingErr> {
        let (actor, _jh) = Actor::spawn(
            Some("CodexOauthWorker".to_string()),
            CodexOauthWorkerActor,
            (handle, cfg),
        )
        .await
        .map_err(|e| {
            ActorProcessingErr::from(format!("CodexOauthWorkerActor spawn failed: {e}"))
        })?;
        Ok(Self { actor })
    }

    /// Submit a credential job (refresh, untrusted ingest, or trusted ingest) for processing.
    pub fn submit(&self, job: CredentialJob) -> Result<(), PolluxError> {
        ractor::cast!(self.actor, CodexOauthWorkerMessage(job)).map_err(|e| {
            PolluxError::RactorError(format!("CodexOauthWorkerActor cast failed: {e}"))
        })
    }
}

/// Actor message wrapping a single credential job.
///
/// Job dispatch is driven by [`CredentialJobKind`] inside the job itself,
/// so a single message variant is sufficient.
#[derive(Debug)]
struct CodexOauthWorkerMessage(CredentialJob);

struct CodexOauthWorkerState {
    job_tx: mpsc::Sender<CredentialJob>,
    handle: CodexActorHandle,
}

struct CodexOauthWorkerActor;

#[ractor::async_trait]
impl Actor for CodexOauthWorkerActor {
    type Msg = CodexOauthWorkerMessage;
    type State = CodexOauthWorkerState;
    type Arguments = (CodexActorHandle, Arc<CodexResolvedConfig>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (handle, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut headers = HeaderMap::new();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30));

        if let Some(proxy_url) = cfg.proxy.clone() {
            let proxy = reqwest::Proxy::all(proxy_url.as_str())
                .expect("invalid proxy url for reqwest client");
            builder = builder.proxy(proxy);
        }

        if cfg.enable_multiplexing {
            builder = builder.http2_adaptive_window(true);
        } else {
            headers.insert(CONNECTION, HeaderValue::from_static("close"));

            builder = builder
                .http1_only()
                .pool_max_idle_per_host(0)
                .pool_idle_timeout(Duration::from_secs(0));
        }

        let client = builder
            .default_headers(headers)
            .build()
            .expect("FATAL: initialize codex credential processor HTTP client failed");

        let oauth_tps = cfg.oauth_tps.max(1);
        let oauth_tps_u32 = u32::try_from(oauth_tps).unwrap_or(u32::MAX);
        let burst_u32 = u32::try_from(oauth_tps.saturating_mul(2)).unwrap_or(u32::MAX);
        let limiter = Arc::new(RateLimiter::direct(
            Quota::per_second(std::num::NonZeroU32::new(oauth_tps_u32).unwrap())
                .allow_burst(std::num::NonZeroU32::new(burst_u32).unwrap()),
        ));

        let (job_tx, job_rx) = mpsc::channel::<CredentialJob>(1000);
        let pipeline_handle = handle.clone();

        let buffer_unordered = oauth_tps.saturating_mul(2).max(1);
        tokio::spawn(async move {
            info!(
                "Codex Credential Pipeline Started: BufferUnordered={}, RateLimit={}/s, Burst={}",
                buffer_unordered, oauth_tps_u32, burst_u32
            );

            let mut pipeline = ReceiverStream::new(job_rx)
                .ratelimit_stream(&limiter)
                .map(|job| {
                    let http = client.clone();
                    async move { job.execute(http).await }
                })
                .buffer_unordered(buffer_unordered);

            while let Some(result) = pipeline.next().await {
                if let Err(e) = pipeline_handle.send_process_complete(result) {
                    warn!("Actor unreachable (channel closed), worker stopping: {}", e);
                    break;
                }
            }

            info!("Codex Credential Pipeline Stopped");
        });

        info!(
            proxy = %cfg.proxy.as_ref().map_or("<none>", |u| u.as_str()),
            enable_multiplexing = cfg.enable_multiplexing,
            oauth_tps = cfg.oauth_tps,
            "CodexCredentialProcessor runtime config loaded"
        );

        Ok(CodexOauthWorkerState { job_tx, handle })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        CodexOauthWorkerMessage(job): Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let tx = state.job_tx.clone();
        let handle = state.handle.clone();

        tokio::spawn(async move {
            send_job(tx, handle, job).await;
        });

        Ok(())
    }
}

async fn send_job(tx: mpsc::Sender<CredentialJob>, handle: CodexActorHandle, job: CredentialJob) {
    if let Err(e) = tx.send(job).await {
        warn!(
            "Failed to submit credential job (channel closed/full): {}",
            e
        );
        let result = Err(CredentialProcessError {
            original_job: e.0,
            error: PolluxError::RactorError("CodexOauthWorker job queue is closed".to_string()),
        });
        if let Err(e) = handle.send_process_complete(result) {
            warn!(
                "Actor unreachable (channel closed), dropping credential process result: {}",
                e
            );
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::providers::codex) struct CredentialJob {
    pub cred: CodexResource,
    pub kind: CredentialJobKind,
}

impl CredentialJob {
    pub(in crate::providers::codex) fn refresh(id: CredentialId, cred: CodexResource) -> Self {
        Self {
            cred,
            kind: CredentialJobKind::Refresh(id),
        }
    }

    pub(in crate::providers::codex) fn ingest_untrusted_seed(
        seed: &CodexRefreshTokenSeed,
    ) -> Result<Self, PolluxError> {
        let mut cred = CodexResource::default();
        cred.update_credential(json!({ "refresh_token": seed.refresh_token() }))?;
        Ok(Self {
            cred,
            kind: CredentialJobKind::IngestUntrusted,
        })
    }

    pub(in crate::providers::codex) fn ingest_trusted_oauth(
        token_response: &OauthTokenResponse,
    ) -> Result<Self, PolluxError> {
        let cred = CodexResource::try_from_oauth_token_response(&token_response, None)?;
        Ok(Self {
            cred,
            kind: CredentialJobKind::IngestTrusted,
        })
    }

    /// Execute the credential job (refresh or ingest) and return the updated job on success.
    ///
    /// Wraps [`execute_inner`] so that all `PolluxError` variants are uniformly
    /// converted into `CredentialProcessError` carrying the original job.
    async fn execute(mut self, client: reqwest::Client) -> CredentialProcessResult {
        match self.execute_inner(client).await {
            Ok(()) => Ok(self),
            Err(error) => Err(CredentialProcessError {
                original_job: self,
                error,
            }),
        }
    }

    /// Core processing logic for a single credential job.
    ///
    /// Dispatches to the appropriate refresh/ingest path, then validates that
    /// the resulting credential contains the required fields (access token,
    /// identity, and refresh token).
    async fn execute_inner(&mut self, client: reqwest::Client) -> Result<(), PolluxError> {
        match self.kind {
            CredentialJobKind::Refresh(_) => {
                refresh_credential(client, *OAUTH_RETRY_POLICY, &mut self.cred, None).await?;
            }
            CredentialJobKind::IngestUntrusted => {
                let refresh_token = self.cred.refresh_token().trim().to_string();
                let refresh_seed = CodexRefreshTokenSeed::new(&refresh_token).ok_or_else(|| {
                    PolluxError::UnexpectedError(
                        "Missing refresh_token for untrusted Codex credential ingest".to_string(),
                    )
                })?;

                refresh_credential(
                    client,
                    *OAUTH_RETRY_POLICY,
                    &mut self.cred,
                    Some(refresh_seed),
                )
                .await?;
            }
            CredentialJobKind::IngestTrusted => {}
        }

        if self.cred.access_token().trim().is_empty() {
            return Err(PolluxError::MissingAccessToken);
        }

        if self.cred.sub().trim().is_empty() || self.cred.account_id().trim().is_empty() {
            let message = match self.kind {
                CredentialJobKind::Refresh(_) => "Missing Codex identity after refresh",
                CredentialJobKind::IngestUntrusted => {
                    "Missing Codex identity after untrusted credential ingest"
                }
                CredentialJobKind::IngestTrusted => {
                    "Missing Codex identity in trusted OAuth response"
                }
            };
            return Err(PolluxError::UnexpectedError(message.to_string()));
        }

        if self.cred.refresh_token().trim().is_empty() {
            return Err(PolluxError::UnexpectedError(
                "Missing refresh_token in Codex credential".to_string(),
            ));
        }

        Ok(())
    }
}

/// Refresh a Codex credential via the OAuth token endpoint.
///
/// When `refresh_seed` is `Some`, the credential is rebuilt from scratch using
/// the full token response (untrusted ingest path). When `None`, only the
/// token-related fields are patched in place, preserving existing identity.
async fn refresh_credential(
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    creds: &mut CodexResource,
    refresh_seed: Option<CodexRefreshTokenSeed>,
) -> Result<(), PolluxError> {
    let refresh_token = match &refresh_seed {
        Some(seed) => seed.refresh_token(),
        None => creds.refresh_token(),
    };
    let token_response = request_token_refresh(client, retry_policy, refresh_token).await?;

    if let Some(seed) = refresh_seed {
        *creds = CodexResource::try_from_oauth_token_response(&token_response, Some(&seed))?;
    } else {
        creds.update_credential(&token_response)?;
        debug!(account_id = %creds.account_id(), "Access token refreshed successfully");
    }
    Ok(())
}

async fn request_token_refresh(
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    refresh_token: &str,
) -> Result<OauthTokenResponse, PolluxError> {
    (|| async { CodexOauthEndpoints::refresh_access_token(refresh_token, client.clone()).await })
        .retry(retry_policy)
        .when(|e: &OauthError| e.is_retryable())
        .notify(|err, dur: Duration| {
            error!(
                "Codex OAuth2 refresh retrying error {} with sleeping {:?}",
                err.to_string(),
                dur
            );
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;

    fn make_test_jwt(payload: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload_bytes = serde_json::to_vec(payload).expect("serialize payload");
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_bytes);
        format!("{header}.{payload_b64}.sig")
    }

    #[test]
    fn refresh_job_keeps_refresh_id() {
        let cred = CodexResource::from_payload(json!({
            "account_id": "acct-1",
            "sub": "sub-1",
            "refresh_token": "rt-1",
            "access_token": "at-1",
            "expiry": "2026-01-01T00:00:00Z",
        }))
        .expect("valid credential payload");

        let job = CredentialJob::refresh(42, cred);

        assert_eq!(job.kind.credential_id(), Some(42));
    }

    #[test]
    fn untrusted_ingest_job_sets_refresh_token() {
        let seed = CodexRefreshTokenSeed::new("seed-rt").expect("valid seed");

        let job = CredentialJob::ingest_untrusted_seed(&seed).expect("ingest job");

        assert!(matches!(job.kind, CredentialJobKind::IngestUntrusted));
        assert_eq!(job.cred.refresh_token(), "seed-rt");
        assert_eq!(job.kind.credential_id(), None);
    }

    #[test]
    fn trusted_ingest_job_skips_network_work_when_identity_is_present() {
        let id_token = make_test_jwt(&json!({
            "sub": "auth0|trusted-sub",
            "email": "trusted@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-trusted",
                "chatgpt_plan_type": "plus",
            }
        }));

        let token_response: OauthTokenResponse = serde_json::from_value(json!({
            "access_token": "trusted-at",
            "token_type": "bearer",
            "expires_in": 3600,
            "refresh_token": "trusted-rt",
            "id_token": id_token,
        }))
        .expect("token response deserializes");

        let job =
            CredentialJob::ingest_trusted_oauth(&token_response).expect("trusted oauth ingest job");

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let completed = runtime
            .block_on(job.execute(reqwest::Client::new()))
            .expect("trusted oauth ingest succeeds without refresh");

        assert!(matches!(completed.kind, CredentialJobKind::IngestTrusted));
        assert_eq!(completed.cred.account_id(), "acct-trusted");
        assert_eq!(completed.cred.sub(), "auth0|trusted-sub");
        assert_eq!(completed.cred.access_token(), "trusted-at");
        assert_eq!(completed.cred.refresh_token(), "trusted-rt");
        assert_eq!(completed.cred.email(), Some("trusted@example.com"));
        assert_eq!(completed.cred.chatgpt_plan_type(), Some("plus"));
    }
}
