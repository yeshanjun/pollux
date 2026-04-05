use crate::error::{CodexError, IsRetryable};
use crate::providers::codex::CodexActorHandle;
use crate::providers::provider_endpoints::ProviderEndpoints;
use crate::providers::upstream_retry::post_json_with_retry;
use crate::providers::{ActionForError, policy::classify_upstream_error};
use crate::server::routes::codex::headers::{CodexRequestHeaders, OpenaiRequestHeaders};
use crate::utils::logging::with_pretty_json_debug;
use backon::{ExponentialBuilder, Retryable};
use pollux_schema::{CodexErrorBody, CodexRequestBody};
use reqwest::header::{HeaderName, HeaderValue};

use std::time::{Duration, Instant};
use tracing::{debug, info};
use url::Url;

/// Minimal passthrough client for Codex upstream.
///
/// Notes:
/// - Schema conversion is handled by the router; this client only serializes and forwards JSON.
/// - OAuth/token refresh is intentionally left as future work (placeholders in config).
#[derive(Clone)]
pub(crate) struct CodexClient {
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    endpoints: ProviderEndpoints,
    trace_header: Option<String>,
}

impl CodexClient {
    pub(crate) fn new(
        client: reqwest::Client,
        base_url: Url,
        retry_max_times: usize,
        trace_header: Option<String>,
    ) -> Self {
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO)
            .with_max_times(retry_max_times);
        let endpoints = Self::endpoints_for_base(base_url);
        info!(endpoint = %endpoints.select(false), "CodexClient initialized");

        Self {
            client,
            retry_policy,
            endpoints,
            trace_header,
        }
    }

    fn endpoints_for_base(base: Url) -> ProviderEndpoints {
        ProviderEndpoints::new(
            base,
            "./backend-api/codex/responses",
            None,
            "./backend-api/codex/responses",
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_codex(
        &self,
        handle: &CodexActorHandle,
        model: &str,
        model_mask: u64,
        route_key: Option<u64>,
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
        let trace_header = self.trace_header.clone();

        let op = move || {
            let handle = handle.clone();
            let client = client.clone();
            let endpoints = endpoints.clone();
            let body = body.clone();
            let model = model.clone();
            let inbound_headers = inbound_headers.clone();
            let trace_header = trace_header.clone();
            async move {
                let start = Instant::now();
                let lease = handle
                    .get_credential(model_mask, route_key)
                    .await?
                    .ok_or(CodexError::NoAvailableCredential)?;

                let waited_us = start.elapsed().as_micros() as u64;
                info!(
                    waited_us,
                    id = lease.id,
                    model = %model,
                    stream = client_stream,
                    "[Codex] Lease acquired"
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
                debug!(codex_headers = ?codex_headers, "[Codex] Prepared upstream headers for request");
                let mut upstream_headers = codex_headers.into_header_map();

                if let Some(ref header_name) = trace_header {
                    let email = lease.email.as_deref().unwrap_or("unknown");
                    let trace_value = format!("codex:{}:{}", email, lease.id);
                    if let (Ok(name), Ok(val)) = (
                        HeaderName::from_bytes(header_name.as_bytes()),
                        HeaderValue::from_str(&trace_value),
                    ) {
                        upstream_headers.insert(name, val);
                    }
                }

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
