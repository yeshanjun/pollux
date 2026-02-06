use crate::config::CodexResolvedConfig;
use crate::error::{CodexError, IsRetryable};
use crate::providers::codex::{CODEX_RESPONSES_URL, CodexActorHandle};
use crate::providers::{ActionForError, policy::classify_upstream_error};
use backon::{ExponentialBuilder, Retryable};
use pollux_schema::{CodexErrorBody, CodexRequestBody};

use std::time::{Duration, Instant};
use tracing::info;

use super::api::CodexApi;

/// Minimal passthrough client for Codex upstream.
///
/// Notes:
/// - Schema conversion is handled by the router; this client only serializes and forwards JSON.
/// - OAuth/token refresh is intentionally left as future work (placeholders in config).
pub(crate) struct CodexClient {
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
}

impl CodexClient {
    pub(crate) fn new(cfg: &CodexResolvedConfig, client: reqwest::Client) -> Self {
        let max_attempts = cfg.retry_max_times.max(1);
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_millis(300))
            .with_max_times(max_attempts)
            .with_jitter();

        Self {
            client,
            retry_policy,
        }
    }

    pub(crate) async fn call_codex(
        &self,
        handle: &CodexActorHandle,
        model: &str,
        model_mask: u64,
        client_stream: bool,
        body: &CodexRequestBody,
    ) -> Result<reqwest::Response, CodexError> {
        let handle = handle.clone();
        let client = self.client.clone();
        let responses_url = CODEX_RESPONSES_URL.clone();
        let retry_policy_inner = self.retry_policy;
        let body = body.clone();
        let model = model.to_string();

        let op = move || {
            let handle = handle.clone();
            let client = client.clone();
            let responses_url = responses_url.clone();
            let body = body.clone();
            let model = model.clone();
            async move {
                let start = Instant::now();
                let lease = handle
                    .get_credential(model_mask)
                    .await?
                    .ok_or(CodexError::NoAvailableCredential)?;

                let actor_took = start.elapsed();
                info!(
                    channel = "codex",
                    lease.id = lease.id,
                    lease.waited_us = actor_took.as_micros() as u64,
                    req.model = %model,
                    req.stream = client_stream,

                    "[Codex] [ID: {}] [{:?}] Post responses -> {}",
                    lease.id,
                    actor_took,
                    model
                );

                let resp = CodexApi::try_post_codex(
                    client.clone(),
                    responses_url.clone(),
                    &lease,
                    &body,
                    retry_policy_inner,
                )
                .await?;

                if resp.status().is_success() {
                    return Ok(resp);
                }

                let status = resp.status();
                let (action, final_error) = classify_upstream_error(
                    resp,
                    |json: CodexErrorBody| CodexError::UpstreamMappedError { status, body: json },
                    |status, body| CodexError::UpstreamFallbackError { status, body },
                )
                .await;

                match &action {
                    ActionForError::RateLimit(duration) => {
                        handle
                            .report_rate_limit(lease.id, model_mask, *duration)
                            .await;
                        // Optionally, could add a log here about when to retry
                    }
                    ActionForError::Ban => {
                        handle.report_baned(lease.id).await;
                    }
                    ActionForError::ModelUnsupported => {
                        handle.report_model_unsupported(lease.id, model_mask).await;
                    }
                    ActionForError::Invalid => {
                        handle.report_invalid(lease.id).await;
                    }
                    ActionForError::None => {
                        // Do nothing
                    }
                }

                match &final_error {
                    CodexError::UpstreamMappedError { status, .. } => {
                        tracing::warn!(
                            lease_id = lease.id,
                            model = %model,
                            status = %status,
                            action = ?action,
                            "[Codex] Upstream mapped error"
                        );
                    }
                    CodexError::UpstreamFallbackError { status, .. } => {
                        tracing::warn!(
                            lease_id = lease.id,
                            model = %model,
                            status = %status,
                            action = ?action,
                            "[Codex] Upstream fallback error"
                        );
                    }
                    CodexError::Reqwest(error) => {
                        tracing::warn!(
                            lease_id = lease.id,
                            model = %model,
                            status = ?error.status(),
                            action = ?action,
                            "[Codex] Upstream reqwest error"
                        );
                    }
                    _ => {
                        tracing::warn!(
                            lease_id = lease.id,
                            model = %model,
                            status = "N/A",
                            action = ?action,
                            "[Codex] Upstream other error"
                        );
                    }
                }

                Err(final_error)
            }
        };

        op.retry(&self.retry_policy)
            .when(|err: &CodexError| err.is_retryable())
            .notify(|err, dur: Duration| {
                tracing::warn!("Codex retrying after error {} in {:?}", err, dur);
            })
            .await
    }
}
