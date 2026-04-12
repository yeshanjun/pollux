use super::super::{
    client::oauth::{
        OAUTH_RETRY_POLICY,
        endpoints::GoogleOauthEndpoints,
        ops::GoogleOauthOps,
        types::{LoadCodeAssistResponse, OnboardOperationResponse, UserTier},
        utils::attach_email_from_id_token,
    },
    manager::{CredentialId, GeminiCliActorHandle},
    resource::GeminiCliResource,
};
use crate::config::GeminiCliResolvedConfig;
use crate::error::{IsRetryable, OauthError, PolluxError};
use backon::{ExponentialBuilder, Retryable};
use futures::stream::StreamExt;
use governor::{Quota, RateLimiter};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::header::{CONNECTION, HeaderMap, HeaderValue};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
pub(in crate::providers::geminicli) struct CredentialJob {
    pub cred: GeminiCliResource,
    pub kind: CredentialJobKind,
}

impl CredentialJob {
    async fn execute(mut self, client: reqwest::Client) -> CredentialProcessResult {
        match self.kind {
            CredentialJobKind::Refresh(_) => {
                if let Err(e) =
                    refresh_inner(client, *OAUTH_RETRY_POLICY, &mut self.cred, false).await
                {
                    return Err(CredentialProcessError {
                        original_job: self,
                        error: e,
                    });
                }
            }
            CredentialJobKind::Ingest => {
                if (self.cred.access_token().is_empty()
                    || self.cred.is_expired()
                    || self.cred.sub().is_empty())
                    && let Err(e) =
                        refresh_inner(client.clone(), *OAUTH_RETRY_POLICY, &mut self.cred, true)
                            .await
                {
                    return Err(CredentialProcessError {
                        original_job: self,
                        error: e,
                    });
                }

                if self.cred.sub().is_empty() {
                    return Err(CredentialProcessError {
                        original_job: self,
                        error: PolluxError::UnexpectedError(
                            "Missing sub in id_token claims".into(),
                        ),
                    });
                }

                let token_str = self.cred.access_token();
                if token_str.is_empty() {
                    return Err(CredentialProcessError {
                        original_job: self,
                        error: PolluxError::MissingAccessToken,
                    });
                }

                match ensure_companion_project(token_str, client).await {
                    Ok(project_id) => {
                        self.cred.set_project_id(project_id);
                    }
                    Err(e) => {
                        return Err(CredentialProcessError {
                            original_job: self,
                            error: e,
                        });
                    }
                }
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CredentialJobKind {
    Refresh(CredentialId),
    Ingest,
}

impl CredentialJobKind {
    pub fn credential_id(&self) -> Option<CredentialId> {
        match self {
            CredentialJobKind::Refresh(id) => Some(*id),
            CredentialJobKind::Ingest => None,
        }
    }
}

pub struct CredentialProcessError {
    pub original_job: CredentialJob,
    pub error: PolluxError,
}

pub type CredentialProcessResult = Result<CredentialJob, CredentialProcessError>;

/// Actor message wrapping a single credential job.
///
/// Job dispatch is driven by [`CredentialJobKind`] inside the job itself,
/// so a single message variant is sufficient.
#[derive(Debug)]
struct GeminiCliOauthWorkerMessage(CredentialJob);

/// Handle for submitting credential processing jobs to the Gemini CLI worker actor.
#[derive(Clone)]
pub(in crate::providers::geminicli) struct GeminiCliOauthWorkerHandle {
    actor: ActorRef<GeminiCliOauthWorkerMessage>,
}

impl GeminiCliOauthWorkerHandle {
    pub async fn spawn(
        handle: GeminiCliActorHandle,
        cfg: Arc<GeminiCliResolvedConfig>,
    ) -> Result<Self, ActorProcessingErr> {
        let (actor, _jh) = Actor::spawn(
            Some("GeminiCliOauthWorker".to_string()),
            GeminiCliOauthWorkerActor,
            (handle, cfg),
        )
        .await
        .map_err(|e| {
            ActorProcessingErr::from(format!("GeminiCliOauthWorkerActor spawn failed: {e}"))
        })?;
        Ok(Self { actor })
    }

    /// Submit a credential job (refresh or onboard) for processing.
    pub fn submit(&self, job: CredentialJob) -> Result<(), PolluxError> {
        ractor::cast!(self.actor, GeminiCliOauthWorkerMessage(job)).map_err(|e| {
            PolluxError::RactorError(format!("GeminiCliOauthWorkerActor cast failed: {e}"))
        })
    }
}

async fn ensure_companion_project(
    access_token: &str,
    client: reqwest::Client,
) -> Result<String, PolluxError> {
    let load_json =
        GoogleOauthOps::load_code_assist_with_retry(access_token, client.clone()).await?;
    debug!(body = %load_json, "loadCodeAssist upstream body");

    let load_resp: LoadCodeAssistResponse =
        serde_json::from_value(load_json.clone()).map_err(PolluxError::JsonError)?;

    load_resp.ensure_eligible(load_json)?;

    let tier = load_resp.resolve_effective_tier();

    if let Some(existing_project_id) = load_resp.cloudaicompanion_project {
        info!(
            project_id = %existing_project_id,
            tier = %tier.as_str(),
            "loadCodeAssist resolved companion project id"
        );
        return Ok(existing_project_id);
    }

    info!(
        tier = %tier.as_str(),
        "No existing companion project found; starting onboarding"
    );
    let new_project_id = perform_onboarding(access_token, tier, client).await?;

    info!(
        project_id = %new_project_id,
        "Companion project provisioning completed"
    );
    Ok(new_project_id)
}

async fn perform_onboarding(
    access_token: &str,
    tier: UserTier,
    client: reqwest::Client,
) -> Result<String, PolluxError> {
    const MAX_ATTEMPTS: usize = 5;
    const RETRY_DELAY: Duration = Duration::from_secs(5);
    let mut last_resp: Option<serde_json::Value> = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let resp_json = GoogleOauthOps::onboard_code_assist_with_retry(
            access_token,
            tier.clone(),
            None,
            client.clone(),
        )
        .await?;
        debug!(body = %resp_json, "onboardCodeAssist upstream body");

        last_resp = Some(resp_json.clone());
        let op_resp: OnboardOperationResponse =
            serde_json::from_value(resp_json.clone()).map_err(PolluxError::JsonError)?;

        if op_resp.done {
            return op_resp
                .response
                .and_then(|r| r.project_details)
                .map(|p| p.id)
                .ok_or_else(|| {
                    OauthError::Flow {
                        code: "ONBOARD_FAILED".to_string(),
                        message: "Onboarding completed but returned no project ID".to_string(),
                        details: Some(resp_json),
                    }
                    .into()
                });
        }

        if attempt < MAX_ATTEMPTS {
            info!(
                "onboardCodeAssist pending (attempt {}/{}), retrying in {:?}...",
                attempt, MAX_ATTEMPTS, RETRY_DELAY
            );
            sleep(RETRY_DELAY).await;
        }
    }

    Err(OauthError::Flow {
        code: "ONBOARD_TIMEOUT".to_string(),
        message: "Companion project provisioning timed out".to_string(),
        details: last_resp,
    }
    .into())
}

struct GeminiCliOauthWorkerState {
    job_tx: mpsc::Sender<CredentialJob>,
    handle: GeminiCliActorHandle,
}

struct GeminiCliOauthWorkerActor;

#[ractor::async_trait]
impl Actor for GeminiCliOauthWorkerActor {
    type Msg = GeminiCliOauthWorkerMessage;
    type State = GeminiCliOauthWorkerState;
    type Arguments = (GeminiCliActorHandle, Arc<GeminiCliResolvedConfig>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (handle, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut headers = HeaderMap::new();
        let mut builder = reqwest::Client::builder()
            .user_agent(crate::providers::geminicli::GOOGLE_AUTH_LIB_USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15));
        if let Some(proxy_url) = cfg.proxy.clone() {
            let proxy = reqwest::Proxy::all(proxy_url.as_str())
                .expect("invalid proxy url for reqwest client");
            builder = builder.proxy(proxy);
        }
        if !cfg.enable_multiplexing {
            headers.insert(CONNECTION, HeaderValue::from_static("close"));

            builder = builder
                .http1_only()
                .pool_max_idle_per_host(0)
                .pool_idle_timeout(Duration::from_secs(0));
        } else {
            builder = builder.http2_adaptive_window(true);
        }
        let client = builder
            .default_headers(headers)
            .build()
            .expect("FATAL: initialize credential processor HTTP client failed");
        let oauth_tps = cfg.oauth_tps.max(1);
        let oauth_tps_u32 = u32::try_from(oauth_tps).unwrap_or(u32::MAX);
        let burst_u32 = u32::try_from(oauth_tps.saturating_mul(2)).unwrap_or(u32::MAX);
        let limiter = Arc::new(RateLimiter::direct(
            Quota::per_second(std::num::NonZeroU32::new(oauth_tps_u32).unwrap())
                .allow_burst(std::num::NonZeroU32::new(burst_u32).unwrap()),
        ));

        let (job_tx, job_rx) = mpsc::channel::<CredentialJob>(1000);
        let pipeline_handle = handle.clone();

        // Spawn background credential worker using buffer_unordered semantics.
        let buffer_unordered = oauth_tps.saturating_mul(2).max(1);
        tokio::spawn(async move {
            info!(
                "GeminiCli Credential Pipeline Started: BufferUnordered={}, RateLimit={}/s, Burst={}",
                buffer_unordered, oauth_tps_u32, burst_u32
            );

            let mut pipeline = ReceiverStream::new(job_rx)
                .map(|job| {
                    let lim = limiter.clone();
                    let http = client.clone();
                    async move {
                        lim.until_ready().await;
                        job.execute(http).await
                    }
                })
                .buffer_unordered(buffer_unordered);

            while let Some(outcome) = pipeline.next().await {
                if let Err(e) = pipeline_handle.send_process_complete(outcome) {
                    warn!("Actor unreachable (channel closed), worker stopping: {}", e);
                    break;
                }
            }
            info!("GeminiCli Credential Pipeline Stopped");
        });

        info!(
            proxy = %cfg.proxy.as_ref().map_or("<none>", url::Url::as_str),
            enable_multiplexing = cfg.enable_multiplexing,
            oauth_tps = cfg.oauth_tps,
            "GeminiCliOauthWorker runtime config loaded"
        );

        Ok(GeminiCliOauthWorkerState { job_tx, handle })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        GeminiCliOauthWorkerMessage(job): Self::Msg,
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

async fn send_job(
    tx: mpsc::Sender<CredentialJob>,
    handle: GeminiCliActorHandle,
    job: CredentialJob,
) {
    if let Err(e) = tx.send(job).await {
        warn!(
            "Failed to submit credential job (channel closed/full): {}",
            e
        );
        let result = Err(CredentialProcessError {
            original_job: e.0,
            error: PolluxError::RactorError("GeminiCliOauthWorker job queue is closed".to_string()),
        });
        if let Err(e) = handle.send_process_complete(result) {
            warn!(
                "Actor unreachable (channel closed), dropping credential process result: {}",
                e
            );
        }
    }
}

/// Shared refresh implementation so both direct calls and the background
/// worker use the same logic.
pub async fn refresh_inner(
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    creds: &mut GeminiCliResource,
    attach_email: bool,
) -> Result<(), PolluxError> {
    let payload = (|| async {
        GoogleOauthEndpoints::refresh_access_token(creds.refresh_token(), client.clone()).await
    })
    .retry(retry_policy)
    .when(|e: &OauthError| e.is_retryable())
    .notify(|err, dur: Duration| {
        error!(
            "Google Oauth2 Retrying Error {} with sleeping {:?}",
            err.to_string(),
            dur
        );
    })
    .await?;
    let payload: Value = serde_json::to_value(&payload)?;
    apply_refresh_payload(creds, payload, attach_email)?;
    info!(
        project_id = %creds.project_id(),
        "Access token refreshed successfully"
    );
    Ok(())
}

fn apply_refresh_payload(
    creds: &mut GeminiCliResource,
    mut payload: Value,
    attach_email: bool,
) -> Result<(), PolluxError> {
    debug!("Token response payload: {}", payload);
    if attach_email {
        // Attach optional email from the ID token before persisting credentials.
        attach_email_from_id_token(&mut payload);
    }
    creds.update_credential(&payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use chrono::{Duration, Utc};
    use serde_json::json;

    fn make_test_jwt(payload: &Value) -> String {
        // Signature is irrelevant; production code only base64url-decodes the payload.
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload_bytes = serde_json::to_vec(payload).expect("serialize payload");
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_bytes);
        format!("{header}.{payload_b64}.sig")
    }

    fn make_expired_credential() -> GeminiCliResource {
        GeminiCliResource::from_payload(json!({
            "project_id": "project-a",
            "refresh_token": "refresh-token",
            "expiry": Utc::now() - Duration::minutes(10),
        }))
        .expect("valid credential payload")
    }

    #[test]
    fn onboard_payload_updates_token_and_email() {
        let mut cred = make_expired_credential();
        assert!(cred.is_expired());

        let email = "test@example.com";
        let sub = "google-subject-1";
        let access_token = "new-access-token".to_string();
        let id_token = make_test_jwt(&json!({ "email": email, "sub": sub }));
        let payload = json!({
            "access_token": access_token,
            "expires_in": 3600,
            "token_type": "bearer",
            "id_token": id_token,
        });
        apply_refresh_payload(&mut cred, payload, true).expect("refresh payload applied");

        assert!(!cred.is_expired());
        assert_eq!(cred.access_token(), access_token.as_str());
        assert_eq!(cred.email(), Some(email));
        assert_eq!(cred.sub(), sub);
        assert_eq!(cred.project_id(), "project-a");
    }

    #[test]
    fn refresh_payload_preserves_email_without_id_token() {
        let mut cred = GeminiCliResource::from_payload(json!({
            "email": "old@example.com",
            "project_id": "project-b",
            "refresh_token": "refresh-token",
            "expiry": Utc::now() + Duration::minutes(10),
        }))
        .expect("valid credential payload");

        let payload = json!({
            "access_token": "new-token",
            "expires_in": 3600,
            "token_type": "bearer",
        });
        apply_refresh_payload(&mut cred, payload, false).expect("refresh payload applied");

        assert_eq!(cred.email(), Some("old@example.com"));
        assert_eq!(cred.access_token(), "new-token");
    }
}
