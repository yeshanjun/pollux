use crate::config::AntigravityResolvedConfig;
use crate::db::{AntigravityCreate, AntigravityPatch};
use crate::error::{OauthError, PolluxError};
use crate::providers::antigravity::AntigravityActorHandle;
use crate::providers::antigravity::client::oauth::{
    endpoints::AntigravityOauthEndpoints,
    ops::{AntigravityOauthOps, LoadCodeAssistResponse},
};
use chrono::{Duration as ChronoDuration, Utc};
use futures::stream::StreamExt;
use governor::{Quota, RateLimiter};
use oauth2::TokenResponse;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::header::{CONNECTION, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub(crate) struct AntigravityRefreshTokenSeed {
    refresh_token: String,
}

impl AntigravityRefreshTokenSeed {
    pub fn new(refresh_token: String) -> Option<Self> {
        let refresh_token = refresh_token.trim().to_string();
        if refresh_token.is_empty() {
            return None;
        }
        Some(Self { refresh_token })
    }

    pub fn refresh_token(&self) -> &str {
        self.refresh_token.as_str()
    }
}

#[derive(Debug)]
pub(crate) enum RefreshOutcome {
    /// Refresh an existing DB credential (project_id must already exist).
    RefreshCredential {
        id: u64,
        patch: AntigravityPatch,
        result: Result<(), PolluxError>,
    },

    /// Refresh a 0-trust seed and discover a project_id.
    OnboardSeed {
        seed: AntigravityRefreshTokenSeed,
        result: Result<AntigravityCreate, PolluxError>,
    },
}

#[derive(Debug)]
enum RefreshTask {
    RefreshCredential { id: u64, refresh_token: String },
    OnboardSeed { seed: AntigravityRefreshTokenSeed },
}

impl RefreshTask {
    async fn execute(
        self,
        cfg: Arc<AntigravityResolvedConfig>,
        client: reqwest::Client,
    ) -> RefreshOutcome {
        match self {
            Self::RefreshCredential { id, refresh_token } => {
                let result = refresh_existing(cfg, client, refresh_token.as_str()).await;
                match result {
                    Ok(patch) => RefreshOutcome::RefreshCredential {
                        id,
                        patch,
                        result: Ok(()),
                    },
                    Err(e) => RefreshOutcome::RefreshCredential {
                        id,
                        patch: AntigravityPatch::default(),
                        result: Err(e),
                    },
                }
            }
            Self::OnboardSeed { seed } => {
                let result = refresh_and_discover(cfg, client, &seed).await;
                RefreshOutcome::OnboardSeed { seed, result }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Actor-based worker (mirrors geminicli's GeminiCliOauthWorkerActor)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct AntigravityOauthWorkerMessage(RefreshTask);

struct AntigravityOauthWorkerState {
    job_tx: mpsc::Sender<RefreshTask>,
    handle: AntigravityActorHandle,
}

struct AntigravityOauthWorkerActor;

#[ractor::async_trait]
impl Actor for AntigravityOauthWorkerActor {
    type Msg = AntigravityOauthWorkerMessage;
    type State = AntigravityOauthWorkerState;
    type Arguments = (AntigravityActorHandle, Arc<AntigravityResolvedConfig>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (handle, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut headers = HeaderMap::new();
        let mut builder = reqwest::Client::builder()
            .user_agent("antigravity-oauth/1.0".to_string())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30));

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

        let http = builder
            .default_headers(headers)
            .build()
            .expect("FATAL: initialize antigravity refresh HTTP client failed");

        let oauth_tps = cfg.oauth_tps.max(1);
        let oauth_tps_u32 = u32::try_from(oauth_tps).unwrap_or(u32::MAX);
        let burst_u32 = u32::try_from(oauth_tps.saturating_mul(2)).unwrap_or(u32::MAX);
        let limiter = Arc::new(RateLimiter::direct(
            Quota::per_second(std::num::NonZeroU32::new(oauth_tps_u32).unwrap())
                .allow_burst(std::num::NonZeroU32::new(burst_u32).unwrap()),
        ));

        let (job_tx, job_rx) = mpsc::channel::<RefreshTask>(1000);
        let pipeline_handle = handle.clone();

        let buffer_unordered = oauth_tps.saturating_mul(2).max(1);
        let pipeline_cfg = cfg.clone();
        tokio::spawn(async move {
            info!(
                "Antigravity Refresh Pipeline Started: BufferUnordered={}, RateLimit={}/s, Burst={}",
                buffer_unordered, oauth_tps_u32, burst_u32
            );

            let mut pipeline = ReceiverStream::new(job_rx)
                .map(|task| {
                    let lim = limiter.clone();
                    let http = http.clone();
                    let cfg = pipeline_cfg.clone();
                    async move {
                        lim.until_ready().await;
                        task.execute(cfg, http).await
                    }
                })
                .buffer_unordered(buffer_unordered);

            while let Some(outcome) = pipeline.next().await {
                if let Err(e) = pipeline_handle.send_refresh_complete(outcome) {
                    warn!("Actor unreachable (channel closed), worker stopping: {}", e);
                    break;
                }
            }

            info!("Antigravity Refresh Pipeline Stopped");
        });

        info!(
            proxy = %cfg.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            enable_multiplexing = cfg.enable_multiplexing,
            oauth_tps = cfg.oauth_tps,
            "AntigravityOauthWorker runtime config loaded"
        );

        Ok(AntigravityOauthWorkerState { job_tx, handle })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        AntigravityOauthWorkerMessage(task): Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let tx = state.job_tx.clone();
        let handle = state.handle.clone();

        tokio::spawn(async move {
            send_job(tx, handle, task).await;
        });

        Ok(())
    }
}

async fn send_job(
    tx: mpsc::Sender<RefreshTask>,
    handle: AntigravityActorHandle,
    task: RefreshTask,
) {
    if let Err(e) = tx.send(task).await {
        warn!("Failed to submit refresh task (channel closed/full): {}", e);
        let outcome = match e.0 {
            RefreshTask::RefreshCredential { id, .. } => RefreshOutcome::RefreshCredential {
                id,
                patch: AntigravityPatch::default(),
                result: Err(PolluxError::RactorError(
                    "AntigravityOauthWorker job queue is closed".to_string(),
                )),
            },
            RefreshTask::OnboardSeed { seed } => RefreshOutcome::OnboardSeed {
                seed,
                result: Err(PolluxError::RactorError(
                    "AntigravityOauthWorker job queue is closed".to_string(),
                )),
            },
        };
        if let Err(e) = handle.send_refresh_complete(outcome) {
            warn!(
                "Actor unreachable (channel closed), dropping refresh result: {}",
                e
            );
        }
    }
}

/// Handle for submitting refresh/onboarding tasks to the worker actor.
#[derive(Clone, Debug)]
pub(in crate::providers::antigravity) struct AntigravityOauthWorkerHandle {
    actor: ActorRef<AntigravityOauthWorkerMessage>,
}

impl AntigravityOauthWorkerHandle {
    pub async fn spawn(
        handle: AntigravityActorHandle,
        cfg: Arc<AntigravityResolvedConfig>,
    ) -> Result<Self, ActorProcessingErr> {
        let (actor, _jh) = Actor::spawn(
            Some("AntigravityOauthWorker".to_string()),
            AntigravityOauthWorkerActor,
            (handle, cfg),
        )
        .await
        .map_err(|e| {
            ActorProcessingErr::from(format!("AntigravityOauthWorkerActor spawn failed: {e}"))
        })?;
        Ok(Self { actor })
    }

    pub fn submit_refresh(&self, id: u64, refresh_token: String) -> Result<(), PolluxError> {
        ractor::cast!(
            self.actor,
            AntigravityOauthWorkerMessage(RefreshTask::RefreshCredential { id, refresh_token })
        )
        .map_err(|e| PolluxError::RactorError(format!("AntigravityOauthWorker cast failed: {e}")))
    }

    pub fn submit_onboard_seed(
        &self,
        seed: AntigravityRefreshTokenSeed,
    ) -> Result<(), PolluxError> {
        ractor::cast!(
            self.actor,
            AntigravityOauthWorkerMessage(RefreshTask::OnboardSeed { seed })
        )
        .map_err(|e| PolluxError::RactorError(format!("AntigravityOauthWorker cast failed: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Refresh / onboarding logic
// ---------------------------------------------------------------------------

async fn refresh_existing(
    cfg: Arc<AntigravityResolvedConfig>,
    http_client: reqwest::Client,
    refresh_token: &str,
) -> Result<AntigravityPatch, PolluxError> {
    let token =
        AntigravityOauthEndpoints::refresh_access_token(&cfg, refresh_token, http_client).await?;

    let access_token = token.access_token().secret().to_string();

    let expires_in = token.expires_in().unwrap_or(Duration::from_secs(3600));
    let expiry = Utc::now()
        + ChronoDuration::from_std(expires_in).unwrap_or_else(|_| ChronoDuration::seconds(3600));

    Ok(AntigravityPatch {
        refresh_token: None,
        email: None,
        access_token: Some(access_token),
        expiry: Some(expiry),
        status: None,
    })
}

async fn refresh_and_discover(
    cfg: Arc<AntigravityResolvedConfig>,
    http_client: reqwest::Client,
    seed: &AntigravityRefreshTokenSeed,
) -> Result<AntigravityCreate, PolluxError> {
    let token = AntigravityOauthEndpoints::refresh_access_token(
        &cfg,
        seed.refresh_token(),
        http_client.clone(),
    )
    .await?;

    let access_token = token.access_token().secret().to_string();

    let expires_in = token.expires_in().unwrap_or(Duration::from_secs(3600));
    let expiry = Utc::now()
        + ChronoDuration::from_std(expires_in).unwrap_or_else(|_| ChronoDuration::seconds(3600));

    let project_id = ensure_project_id(access_token.as_str(), cfg.as_ref(), http_client).await?;

    Ok(AntigravityCreate {
        email: None,
        sub: None,
        project_id,
        refresh_token: seed.refresh_token().to_string(),
        access_token: Some(access_token),
        expiry,
    })
}

async fn ensure_project_id(
    access_token: &str,
    cfg: &AntigravityResolvedConfig,
    http_client: reqwest::Client,
) -> Result<String, PolluxError> {
    let load_json =
        AntigravityOauthOps::load_code_assist_with_retry(cfg, access_token, http_client.clone())
            .await?;
    debug!(body = %load_json, "antigravity loadCodeAssist upstream body");

    let load_resp: LoadCodeAssistResponse =
        serde_json::from_value(load_json.clone()).map_err(PolluxError::JsonError)?;

    if let Some(pid) = load_resp
        .cloudaicompanion_project
        .clone()
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(pid);
    }

    let tier_id = load_resp
        .allowed_tiers
        .iter()
        .find(|t| t.is_default)
        .and_then(|t| t.id.clone())
        .unwrap_or_else(|| "LEGACY".to_string());

    perform_onboarding(access_token, cfg, tier_id.as_str(), http_client).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnboardUserOperation {
    #[serde(default)]
    done: bool,
    #[serde(default)]
    response: Option<OnboardUserResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnboardUserResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project: Option<ProjectIdOrObject>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ProjectIdOrObject {
    String(String),
    Object { id: String },
}

impl ProjectIdOrObject {
    fn into_id(self) -> Option<String> {
        match self {
            ProjectIdOrObject::String(s) if !s.trim().is_empty() => Some(s),
            ProjectIdOrObject::Object { id } if !id.trim().is_empty() => Some(id),
            _ => None,
        }
    }
}

async fn perform_onboarding(
    access_token: &str,
    cfg: &AntigravityResolvedConfig,
    tier_id: &str,
    http_client: reqwest::Client,
) -> Result<String, PolluxError> {
    const MAX_ATTEMPTS: usize = 5;
    const RETRY_DELAY: Duration = Duration::from_secs(2);
    let mut last_resp: Option<Value> = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let resp_json = AntigravityOauthOps::onboard_user_with_retry(
            cfg,
            access_token,
            tier_id,
            http_client.clone(),
        )
        .await?;
        debug!(body = %resp_json, "antigravity onboardUser upstream body");
        last_resp = Some(resp_json.clone());

        let op: OnboardUserOperation =
            serde_json::from_value(resp_json.clone()).map_err(PolluxError::JsonError)?;
        if op.done {
            return op
                .response
                .and_then(|r| r.project)
                .and_then(ProjectIdOrObject::into_id)
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
                "antigravity onboardUser pending (attempt {}/{}), retrying in {:?}...",
                attempt, MAX_ATTEMPTS, RETRY_DELAY
            );
            sleep(RETRY_DELAY).await;
        }
    }

    Err(OauthError::Flow {
        code: "ONBOARD_TIMEOUT".to_string(),
        message: "Project provisioning timed out".to_string(),
        details: last_resp,
    }
    .into())
}
