use crate::config::CodexResolvedConfig;
use crate::error::{CodexError, IsRetryable};
use crate::providers::codex::CodexActorHandle;
use crate::providers::provider_endpoints::ProviderEndpoints;
use crate::providers::upstream_retry::post_json_with_retry;
use crate::providers::{ActionForError, policy::classify_upstream_error};
use crate::server::routes::codex::headers::{CodexRequestHeaders, OpenaiRequestHeaders};
use crate::utils::logging::with_pretty_json_debug;
use backon::{ExponentialBuilder, Retryable};
use pollux_schema::{CodexErrorBody, CodexRequestBody};

use std::time::{Duration, Instant};
use tracing::info;
use url::Url;

/// Minimal passthrough client for Codex upstream.
///
/// Notes:
/// - Schema conversion is handled by the router; this client only serializes and forwards JSON.
/// - OAuth/token refresh is intentionally left as future work (placeholders in config).
pub(crate) struct CodexClient {
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    endpoints: ProviderEndpoints,
}

impl CodexClient {
    pub(crate) fn new(
        cfg: &CodexResolvedConfig,
        client: reqwest::Client,
        base_url: Option<Url>,
    ) -> Self {
        let max_attempts = cfg.retry_max_times.max(1);
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_millis(300))
            .with_max_times(max_attempts)
            .with_jitter();
        let endpoints = base_url
            .map(Self::endpoints_for_base)
            .unwrap_or_else(Self::default_endpoints);

        Self {
            client,
            retry_policy,
            endpoints,
        }
    }

    fn default_endpoints() -> ProviderEndpoints {
        Self::endpoints_for_base(
            Url::parse("https://chatgpt.com").expect("invalid fixed Codex base URL"),
        )
    }

    fn endpoints_for_base(base: Url) -> ProviderEndpoints {
        ProviderEndpoints::new(
            base,
            "/backend-api/codex/responses",
            None,
            "/backend-api/codex/responses",
            None,
        )
    }

    pub(crate) async fn call_codex(
        &self,
        handle: &CodexActorHandle,
        model: &str,
        model_mask: u64,
        client_stream: bool,
        body: &CodexRequestBody,
        inbound_headers: &OpenaiRequestHeaders,
    ) -> Result<reqwest::Response, CodexError> {
        let handle = handle.clone();
        let client = self.client.clone();
        let endpoints = self.endpoints.clone();
        let body = body.clone();
        let model = model.to_string();
        let inbound_headers = inbound_headers.clone();

        let op = move || {
            let handle = handle.clone();
            let client = client.clone();
            let endpoints = endpoints.clone();
            let body = body.clone();
            let model = model.clone();
            let inbound_headers = inbound_headers.clone();
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

                with_pretty_json_debug(&body, |pretty_payload| {
                    tracing::debug!(
                        channel = "codex",
                        lease.id = lease.id,
                        req.model = %model,
                        req.client_stream = client_stream,
                        req.upstream_stream = body.stream,
                        body = %pretty_payload,
                        "[Codex] Prepared upstream payload"
                    );
                });

                let codex_headers = CodexRequestHeaders::build(&inbound_headers, &lease);
                info!(codex_headers = ?codex_headers, "[Codex] Prepared upstream headers for request");
                let upstream_headers = codex_headers.into_header_map();

                let resp = post_json_with_retry(
                    "Codex",
                    &client,
                    endpoints.select(client_stream),
                    Some(upstream_headers),
                    &body,
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
