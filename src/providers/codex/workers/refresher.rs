use super::super::{
    CodexRefreshTokenSeed,
    client::oauth::endpoints::CodexOauthEndpoints,
    manager::{CodexActorHandle, CredentialId},
    oauth::OauthTokenResponse,
    resource::CodexResource,
};
use crate::config::CodexResolvedConfig;
use crate::error::{IsRetryable, OauthError, PolluxError};
use backon::{ExponentialBuilder, Retryable};
use futures::stream::StreamExt;
use governor::{Quota, RateLimiter};
use oauth2::TokenResponse;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use reqwest::header::{CONNECTION, HeaderMap, HeaderValue};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

#[derive(Debug)]
pub(in crate::providers::codex) enum RefreshOutcome {
    RefreshCredential {
        id: CredentialId,
        cred: CodexResource,
        result: Result<(), PolluxError>,
    },
    InitialOauthTokenResponse {
        seed: CodexRefreshTokenSeed,
        result: Result<OauthTokenResponse, PolluxError>,
    },
}

#[derive(Debug)]
enum CodexRefresherMessage {
    RefreshCredential {
        id: CredentialId,
        cred: CodexResource,
    },
    InitialRefreshCredential {
        seed: CodexRefreshTokenSeed,
    },
}

/// Handle for submitting refresh requests to the Codex refresher actor.
#[derive(Clone)]
pub(in crate::providers::codex) struct CodexRefresherHandle {
    actor: ActorRef<CodexRefresherMessage>,
}

impl CodexRefresherHandle {
    pub async fn spawn(
        handle: CodexActorHandle,
        cfg: Arc<CodexResolvedConfig>,
    ) -> Result<Self, ActorProcessingErr> {
        let (actor, _jh) = Actor::spawn(
            Some("CodexRefresher".to_string()),
            CodexRefresherActor,
            (handle, cfg),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(format!("CodexRefresherActor spawn failed: {e}")))?;
        Ok(Self { actor })
    }

    pub fn submit_refresh(&self, id: CredentialId, cred: CodexResource) -> Result<(), PolluxError> {
        ractor::cast!(
            self.actor,
            CodexRefresherMessage::RefreshCredential { id, cred }
        )
        .map_err(|e| PolluxError::RactorError(format!("CodexRefresherActor cast failed: {e}")))
    }

    pub fn submit_initial_refresh(&self, seed: CodexRefreshTokenSeed) -> Result<(), PolluxError> {
        ractor::cast!(
            self.actor,
            CodexRefresherMessage::InitialRefreshCredential { seed }
        )
        .map_err(|e| PolluxError::RactorError(format!("CodexRefresherActor cast failed: {e}")))
    }
}

#[derive(Debug)]
enum RefreshTask {
    RefreshCredential {
        id: CredentialId,
        cred: CodexResource,
    },
    InitialRefreshCredential {
        seed: CodexRefreshTokenSeed,
    },
}

impl RefreshTask {
    pub async fn execute(self, client: reqwest::Client) -> RefreshOutcome {
        // OAuth refresh: keep it small and deterministic; do not reuse upstream retry_max_times.
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::from_secs(1))
            .with_max_delay(Duration::from_secs(3))
            .with_max_times(3)
            .with_jitter();

        match self {
            Self::RefreshCredential { id, mut cred } => {
                let result = refresh_inner(client, retry_policy, &mut cred).await;
                RefreshOutcome::RefreshCredential { id, cred, result }
            }

            Self::InitialRefreshCredential { seed } => {
                let result =
                    refresh_oauth_token_response(client, retry_policy, seed.refresh_token()).await;
                RefreshOutcome::InitialOauthTokenResponse { seed, result }
            }
        }
    }
}

struct CodexRefresherActorState {
    job_tx: mpsc::Sender<RefreshTask>,
    handle: CodexActorHandle,
}

struct CodexRefresherActor;

#[ractor::async_trait]
impl Actor for CodexRefresherActor {
    type Msg = CodexRefresherMessage;
    type State = CodexRefresherActorState;
    type Arguments = (CodexActorHandle, Arc<CodexResolvedConfig>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (handle, cfg): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut headers = HeaderMap::new();
        let mut builder = reqwest::Client::builder()
            .user_agent("codex-oauth/1.0".to_string())
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

        let client = builder
            .default_headers(headers)
            .build()
            .expect("FATAL: initialize codex refresh HTTP client failed");

        let oauth_tps = cfg.oauth_tps.max(1);
        let oauth_tps_u32 = u32::try_from(oauth_tps).unwrap_or(u32::MAX);
        let burst_u32 = u32::try_from(oauth_tps.saturating_mul(2)).unwrap_or(u32::MAX);
        let limiter = Arc::new(RateLimiter::direct(
            Quota::per_second(std::num::NonZeroU32::new(oauth_tps_u32).unwrap())
                .allow_burst(std::num::NonZeroU32::new(burst_u32).unwrap()),
        ));

        let (job_tx, job_rx) = mpsc::channel::<RefreshTask>(1000);
        let pipeline_handle = handle.clone();

        // Spawn background refresh worker using buffer_unordered semantics.
        let buffer_unordered = oauth_tps.saturating_mul(2).max(1);
        tokio::spawn(async move {
            info!(
                "Codex Refresh Pipeline Started: BufferUnordered={}, RateLimit={}/s, Burst={}",
                buffer_unordered, oauth_tps_u32, burst_u32
            );

            let mut pipeline = ReceiverStream::new(job_rx)
                .map(|task| {
                    let lim = limiter.clone();
                    let http = client.clone();
                    async move {
                        lim.until_ready().await;
                        task.execute(http).await
                    }
                })
                .buffer_unordered(buffer_unordered);

            while let Some(outcome) = pipeline.next().await {
                if let Err(e) = pipeline_handle.send_refresh_complete(outcome) {
                    warn!("Actor unreachable (channel closed), worker stopping: {}", e);
                    break;
                }
            }

            info!("Codex Refresh Pipeline Stopped");
        });

        info!(
            proxy = %cfg.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            enable_multiplexing = cfg.enable_multiplexing,
            oauth_tps = cfg.oauth_tps,
            "CodexRefresher runtime config loaded"
        );

        Ok(CodexRefresherActorState { job_tx, handle })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CodexRefresherMessage::RefreshCredential { id, cred } => {
                let tx = state.job_tx.clone();
                let handle = state.handle.clone();
                let task = RefreshTask::RefreshCredential { id, cred };
                tokio::spawn(async move {
                    if let Err(e) = tx.send(task).await {
                        warn!("Failed to submit refresh job (channel closed/full): {}", e);
                        let outcome = match e.0 {
                            RefreshTask::RefreshCredential { id, cred } => {
                                RefreshOutcome::RefreshCredential {
                                    id,
                                    cred,
                                    result: Err(PolluxError::RactorError(
                                        "Refresh job queue is closed".to_string(),
                                    )),
                                }
                            }
                            RefreshTask::InitialRefreshCredential { seed } => {
                                RefreshOutcome::InitialOauthTokenResponse {
                                    seed,
                                    result: Err(PolluxError::RactorError(
                                        "Refresh job queue is closed".to_string(),
                                    )),
                                }
                            }
                        };
                        if let Err(e) = handle.send_refresh_complete(outcome) {
                            warn!(
                                "Actor unreachable (channel closed), dropping refresh outcome: {}",
                                e
                            );
                        }
                    }
                });
            }

            CodexRefresherMessage::InitialRefreshCredential { seed } => {
                let tx = state.job_tx.clone();
                let handle = state.handle.clone();
                let task = RefreshTask::InitialRefreshCredential { seed };
                tokio::spawn(async move {
                    if let Err(e) = tx.send(task).await {
                        warn!("Failed to submit refresh job (channel closed/full): {}", e);
                        let seed = match e.0 {
                            RefreshTask::InitialRefreshCredential { seed } => seed,
                            RefreshTask::RefreshCredential { .. } => {
                                unreachable!("InitialRefreshCredential send failure only")
                            }
                        };
                        let outcome = RefreshOutcome::InitialOauthTokenResponse {
                            seed,
                            result: Err(PolluxError::RactorError(
                                "Refresh job queue is closed".to_string(),
                            )),
                        };
                        if let Err(e) = handle.send_refresh_complete(outcome) {
                            warn!(
                                "Actor unreachable (channel closed), dropping refresh outcome: {}",
                                e
                            );
                        }
                    }
                });
            }
        }
        Ok(())
    }
}

async fn refresh_inner(
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    creds: &mut CodexResource,
) -> Result<(), PolluxError> {
    let token_response = (|| async {
        CodexOauthEndpoints::refresh_access_token(creds.refresh_token(), client.clone()).await
    })
    .retry(retry_policy)
    .when(|e: &OauthError| e.is_retryable())
    .notify(|err, dur: Duration| {
        error!(
            "Codex OAuth2 refresh retrying error {} with sleeping {:?}",
            err.to_string(),
            dur
        );
    })
    .await?;

    let access_token = token_response.access_token().secret().to_string();
    let expires_in = token_response
        .expires_in()
        .unwrap_or_else(|| Duration::from_secs(60 * 60))
        .as_secs() as i64;

    let mut patch = serde_json::Map::new();
    patch.insert("access_token".to_string(), Value::String(access_token));
    patch.insert("expires_in".to_string(), Value::from(expires_in));

    if let Some(rt) = token_response.refresh_token() {
        patch.insert(
            "refresh_token".to_string(),
            Value::String(rt.secret().to_string()),
        );
    }

    creds.update_credential(Value::Object(patch))?;

    debug!(
        account_id = %creds.account_id(),
        "Access token refreshed successfully"
    );
    Ok(())
}

async fn refresh_oauth_token_response(
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    refresh_token: &str,
) -> Result<OauthTokenResponse, PolluxError> {
    let token_response = (|| async {
        CodexOauthEndpoints::refresh_access_token(refresh_token, client.clone()).await
    })
    .retry(retry_policy)
    .when(|e: &OauthError| e.is_retryable())
    .notify(|err, dur: Duration| {
        error!(
            "Codex OAuth2 refresh retrying error {} with sleeping {:?}",
            err.to_string(),
            dur
        );
    })
    .await?;

    let has_id_token = token_response
        .extra_fields()
        .id_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();

    if !has_id_token {
        return Err(PolluxError::UnexpectedError(
            "Missing id_token in refresh token response".to_string(),
        ));
    }

    Ok(token_response)
}
