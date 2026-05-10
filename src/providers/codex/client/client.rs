use crate::error::{CodexError, IsRetryable};
use crate::providers::codex::CodexActorHandle;
use crate::providers::provider_endpoints::ProviderEndpoints;
use crate::providers::upstream_retry::post_json_bytes_with_retry;
use crate::providers::{ActionForError, policy::classify_upstream_error};
use crate::server::routes::codex::CodexContext;
use crate::server::routes::codex::headers::{CodexRequestHeaders, OpenaiRequestHeaders};
use crate::utils::logging::with_pretty_json_debug;
use axum::body::Bytes;
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
    stream_client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    endpoints: ProviderEndpoints,
    compact_url: Url,
    trace_header: Option<String>,
}

impl CodexClient {
    pub(crate) fn new(
        client: reqwest::Client,
        stream_client: reqwest::Client,
        base_url: &Url,
        retry_max_times: usize,
        trace_header: Option<String>,
    ) -> Self {
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO)
            .with_max_times(retry_max_times);
        let compact_url = Self::compact_url(base_url);
        let endpoints = Self::endpoints_for_base(base_url);
        info!(endpoint = %endpoints.select(false), "CodexClient initialized");

        Self {
            client,
            stream_client,
            retry_policy,
            endpoints,
            compact_url,
            trace_header,
        }
    }

    fn endpoints_for_base(base: &Url) -> ProviderEndpoints {
        ProviderEndpoints::new(
            base,
            "./backend-api/codex/responses",
            None,
            "./backend-api/codex/responses",
            None,
        )
    }

    fn compact_url(base: &Url) -> Url {
        base.join("./backend-api/codex/responses/compact")
            .expect("valid compact endpoint path")
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn call_codex(
        &self,
        handle: &CodexActorHandle,
        ctx: &CodexContext,
        body: &CodexRequestBody,
        inbound_headers: &OpenaiRequestHeaders,
    ) -> Result<reqwest::Response, CodexError> {
        let client = if ctx.stream {
            &self.stream_client
        } else {
            &self.client
        };
        let endpoints = &self.endpoints;
        let trace_header = &self.trace_header;
        let model = &ctx.model;
        let model_mask = ctx.model_mask;
        let stream = ctx.stream;
        let request_body = Bytes::from(serde_json::to_vec(body)?);

        let op = move || {
            let request_body = request_body.clone();
            async move {
                let start = Instant::now();
                let lease = handle
                    .get_credential(model_mask, ctx.route_key)
                    .await?
                    .ok_or(CodexError::NoAvailableCredential)?;

                let waited_us = start.elapsed().as_micros();
                info!(
                    waited_us,
                    id = lease.id,
                    model = %model,
                    stream,
                    "[Codex] Lease acquired"
                );

                with_pretty_json_debug(&body, |pretty_payload| {
                    tracing::debug!(
                        channel = "codex",
                        lease.id = lease.id,
                        req.model = %model,
                        req.stream = stream,
                        req.upstream_stream = body.stream,
                        body = %pretty_payload,
                        "[Codex] Prepared upstream payload"
                    );
                });

                let codex_headers = CodexRequestHeaders::build(inbound_headers, &lease);
                debug!(codex_headers = ?codex_headers, "[Codex] Prepared upstream headers for request");
                let mut upstream_headers = codex_headers.into_header_map();

                if let Some(header_name) = trace_header {
                    let email = lease.email.as_deref().unwrap_or("unknown");
                    let trace_value = format!("codex:{}:{}", email, lease.id);
                    if let (Ok(name), Ok(val)) = (
                        HeaderName::from_bytes(header_name.as_bytes()),
                        HeaderValue::from_str(&trace_value),
                    ) {
                        upstream_headers.insert(name, val);
                    }
                }

                let resp = post_json_bytes_with_retry(
                    "Codex",
                    client,
                    endpoints.select(stream),
                    Some(upstream_headers),
                    request_body,
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
                        handle.report_rate_limit(lease.id, model_mask, *duration);
                        // Optionally, could add a log here about when to retry
                    }
                    ActionForError::Ban => {
                        handle.report_banned(lease.id);
                    }
                    ActionForError::ModelUnsupported => {
                        handle.report_model_unsupported(lease.id, model_mask);
                    }
                    ActionForError::Invalid => {
                        handle.report_invalid(lease.id);
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

    /// Transparent passthrough for the `/v1/responses/compact` endpoint.
    ///
    /// The request body is forwarded as-is to the upstream compact endpoint and
    /// the response is returned without interpretation.
    pub(crate) async fn call_codex_compact(
        &self,
        handle: &CodexActorHandle,
        ctx: &CodexContext,
        body: &serde_json::Value,
        inbound_headers: &OpenaiRequestHeaders,
    ) -> Result<reqwest::Response, CodexError> {
        let client = &self.client;
        let compact_url = &self.compact_url;
        let trace_header = &self.trace_header;
        let model = &ctx.model;
        let model_mask = ctx.model_mask;
        let request_body = Bytes::from(serde_json::to_vec(body)?);

        let op = move || {
            let request_body = request_body.clone();
            async move {
                let start = Instant::now();
                let lease = handle
                    .get_credential(model_mask, ctx.route_key)
                    .await?
                    .ok_or(CodexError::NoAvailableCredential)?;

                let waited_us = start.elapsed().as_micros();
                info!(
                    waited_us,
                    id = lease.id,
                    model = %model,
                    "[Codex] Compact lease acquired"
                );

                let codex_headers = CodexRequestHeaders::build(inbound_headers, &lease);
                let mut upstream_headers = codex_headers.into_header_map();

                if let Some(header_name) = trace_header {
                    let email = lease.email.as_deref().unwrap_or("unknown");
                    let trace_value = format!("codex:{}:{}", email, lease.id);
                    if let (Ok(name), Ok(val)) = (
                        HeaderName::from_bytes(header_name.as_bytes()),
                        HeaderValue::from_str(&trace_value),
                    ) {
                        upstream_headers.insert(name, val);
                    }
                }

                let resp = post_json_bytes_with_retry(
                    "Codex",
                    client,
                    compact_url,
                    Some(upstream_headers),
                    request_body,
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
                        handle.report_rate_limit(lease.id, model_mask, *duration);
                    }
                    ActionForError::Ban => {
                        handle.report_banned(lease.id);
                    }
                    ActionForError::ModelUnsupported => {
                        handle.report_model_unsupported(lease.id, model_mask);
                    }
                    ActionForError::Invalid => {
                        handle.report_invalid(lease.id);
                    }
                    ActionForError::None => {}
                }

                tracing::warn!(
                    lease_id = lease.id,
                    model = %model,
                    status = %status,
                    action = ?action,
                    "[Codex] Compact upstream error"
                );

                Err(final_error)
            }
        };

        op.retry(&self.retry_policy)
            .when(|err: &CodexError| err.is_retryable())
            .notify(|err, dur: Duration| {
                tracing::warn!("Codex compact retrying after error {} in {:?}", err, dur);
            })
            .await
    }
}
