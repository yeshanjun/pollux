use crate::error::{GeminiCliError, GeminiCliErrorBody, IsRetryable};
use crate::providers::geminicli::{GeminiCliActorHandle, GeminiContext};
use crate::providers::policy::classify_upstream_error;
use crate::providers::provider_endpoints::ProviderEndpoints;
use crate::providers::upstream_retry::post_json_with_retry;
use crate::utils::logging::with_pretty_json_debug;
use backon::{ExponentialBuilder, Retryable};
use pollux_schema::{
    gemini::GeminiGenerateContentRequest, geminicli::VertexGenerateContentRequest,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use url::Url;

#[derive(Clone)]
pub(crate) struct GeminiClient {
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    endpoints: ProviderEndpoints,
    trace_header: Option<String>,
}

impl GeminiClient {
    pub fn new(
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
        info!(endpoint = %endpoints.select(false), "GeminiClient initialized");

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
            "./v1internal:streamGenerateContent",
            Some("alt=sse"),
            "./v1internal:generateContent",
            None,
        )
    }

    pub async fn call_gemini_cli(
        &self,
        handle: &GeminiCliActorHandle,
        ctx: &GeminiContext,
        body: &GeminiGenerateContentRequest,
    ) -> Result<reqwest::Response, GeminiCliError> {
        let model = &ctx.model;
        let model_mask = ctx.model_mask;
        let stream = ctx.stream;
        let client = &self.client;
        let endpoints = &self.endpoints;
        let trace_header = &self.trace_header;

        let op = {
            move || async move {
                let start = Instant::now();
                let assigned = handle
                    .get_credential(model_mask)
                    .await?
                    .ok_or(GeminiCliError::NoAvailableCredential)?;

                let waited_us = start.elapsed().as_micros() as u64;
                info!(
                    waited_us,
                    id = assigned.id,
                    model = %model,
                    stream,
                    "[GeminiCli] Lease acquired"
                );

                let payload = VertexGenerateContentRequest {
                    model,
                    project: &assigned.project_id,
                    request: body,
                };

                with_pretty_json_debug(&payload, |pretty_payload| {
                    debug!(
                        channel = "geminicli",
                        lease.id = assigned.id,
                        req.model = %model,
                        req.stream = stream,
                        body = %pretty_payload,
                        "[GeminiCLI] Prepared upstream payload"
                    );
                });

                let mut headers = HeaderMap::new();
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", assigned.access_token))
                        .expect("invalid fixed auth header value"),
                );
                if let Ok(ua) =
                    HeaderValue::from_str(&crate::providers::geminicli::geminicli_user_agent(model))
                {
                    headers.insert(reqwest::header::USER_AGENT, ua);
                }

                if let Some(header_name) = trace_header {
                    let email = assigned.email.as_deref().unwrap_or("unknown");
                    let trace_value = format!("geminicli:{}:{}", email, assigned.id);
                    if let (Ok(name), Ok(val)) = (
                        reqwest::header::HeaderName::from_bytes(header_name.as_bytes()),
                        HeaderValue::from_str(&trace_value),
                    ) {
                        headers.insert(name, val);
                    }
                }

                let resp = post_json_with_retry(
                    "GeminiCLI",
                    client,
                    endpoints.select(stream),
                    Some(headers),
                    &payload,
                )
                .await?;
                if !resp.status().is_success() {
                    let status = resp.status();

                    let (action, final_error) = classify_upstream_error(
                        resp,
                        |json: GeminiCliErrorBody| GeminiCliError::UpstreamMappedError {
                            status,
                            body: json,
                        },
                        |status, body| GeminiCliError::UpstreamFallbackError { status, body },
                    )
                    .await;

                    match &action {
                        crate::providers::ActionForError::RateLimit(duration) => {
                            handle
                                .report_rate_limit(assigned.id, model_mask, *duration)
                                .await;
                            info!(
                                "Project: {}, rate limited, retry in {:?}",
                                assigned.project_id, duration
                            );
                        }
                        crate::providers::ActionForError::Ban => {
                            handle.report_baned(assigned.id).await;
                            info!("Project: {}, banned", assigned.project_id);
                        }
                        crate::providers::ActionForError::ModelUnsupported => {
                            handle
                                .report_model_unsupported(assigned.id, model_mask)
                                .await;
                            info!("Project: {}, model unsupported", assigned.project_id);
                        }
                        crate::providers::ActionForError::Invalid => {
                            handle.report_invalid(assigned.id).await;
                            info!("Project: {}, invalid", assigned.project_id);
                        }
                        crate::providers::ActionForError::None => {}
                    }

                    match &final_error {
                        GeminiCliError::UpstreamMappedError { status, body } => {
                            let variant = if status.as_u16() == 429 {
                                Some(body.rate_limit_variant())
                            } else {
                                None
                            };
                            warn!(
                                lease_id = assigned.id,
                                model = %model,
                                status = %status,
                                action = ?action,
                                variant = variant.map(|v| v.to_string()).as_deref(),
                                "[GeminiCli] Upstream mapped error"
                            );
                        }
                        GeminiCliError::UpstreamFallbackError { status, .. } => {
                            warn!(
                                lease_id = assigned.id,
                                model = %model,
                                status = %status,
                                action = ?action,
                                "[GeminiCli] Upstream fallback error"
                            );
                        }
                        GeminiCliError::Reqwest(error) => {
                            warn!(
                                lease_id = assigned.id,
                                model = %model,
                                status = ?error.status(),
                                action = ?action,
                                "[GeminiCli] Upstream reqwest error"
                            );
                        }
                        _ => {
                            warn!(
                                lease_id = assigned.id,
                                model = %model,
                                status = "N/A",
                                action = ?action,
                                "[GeminiCli] Upstream other error"
                            );
                        }
                    }

                    return Err(final_error);
                }
                Ok(resp)
            }
        };

        op.retry(&self.retry_policy)
            .when(|err: &GeminiCliError| err.is_retryable())
            .notify(|err, dur: Duration| {
                error!(
                    "[GeminiCLI] Upstream Error {} retry after {:?}",
                    err.to_string(),
                    dur
                );
            })
            .await
    }
}
