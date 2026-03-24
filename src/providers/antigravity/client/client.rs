use crate::config::AntigravityResolvedConfig;
use crate::error::{GeminiCliErrorBody, IsRetryable, PolluxError};
use crate::providers::antigravity::AntigravityActorHandle;
use crate::providers::policy::classify_upstream_error;
use crate::providers::provider_endpoints::ProviderEndpoints;
use crate::providers::upstream_retry::post_json_with_retry;
use crate::utils::logging::with_pretty_json_debug;
use backon::{ExponentialBuilder, Retryable};
use chrono::Utc;
use pollux_schema::{
    antigravity::AntigravityRequestMeta, gemini::GeminiGenerateContentRequest,
    gemini::GenerationConfig,
};
use rand::Rng as _;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;

const REQUEST_ID_PREFIX: &str = "agent";
const SESSION_ID_MAX_EXCLUSIVE: i64 = 9_000_000_000_000_000_000;
const CLAUDE_THINKING_BUDGET: u32 = 8096;

#[derive(Debug, Clone)]
pub struct AntigravityContext {
    pub model: String,
    pub stream: bool,
    pub path: String,
    pub model_mask: u64,
}

pub struct AntigravityClient {
    client: reqwest::Client,
    retry_policy: ExponentialBuilder,
    endpoints: ProviderEndpoints,
    claude_system_preamble: String,
}

impl AntigravityClient {
    pub fn new(
        cfg: &AntigravityResolvedConfig,
        client: reqwest::Client,
        base_url: Option<Url>,
    ) -> Self {
        let retry_policy = ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_millis(300))
            .with_max_times(cfg.retry_max_times)
            .with_jitter();
        let endpoints = base_url
            .map(Self::endpoints_for_base)
            .unwrap_or_else(Self::default_endpoints);

        Self {
            client,
            retry_policy,
            endpoints,
            claude_system_preamble: cfg.claude_system_preamble.clone(),
        }
    }

    fn default_endpoints() -> ProviderEndpoints {
        Self::endpoints_for_base(
            Url::parse("https://daily-cloudcode-pa.googleapis.com")
                .expect("invalid fixed Antigravity base URL"),
        )
    }

    fn endpoints_for_base(base: Url) -> ProviderEndpoints {
        ProviderEndpoints::new(
            base,
            "/v1internal:streamGenerateContent",
            Some("alt=sse"),
            "/v1internal:generateContent",
            None,
        )
    }

    pub async fn call_antigravity(
        &self,
        handle: &AntigravityActorHandle,
        ctx: &AntigravityContext,
        body: &GeminiGenerateContentRequest,
    ) -> Result<reqwest::Response, PolluxError> {
        let handle = handle.clone();
        let client = self.client.clone();
        let endpoints = self.endpoints.clone();
        let stream = ctx.stream;
        let model = ctx.model.clone();
        let model_mask = ctx.model_mask;
        let path = ctx.path.clone();
        let gemini_request = body.clone();
        let claude_system_preamble = self.claude_system_preamble.clone();

        let op = {
            let gemini_request = gemini_request.clone();
            let claude_system_preamble = claude_system_preamble.clone();
            move || {
                let handle = handle.clone();
                let client = client.clone();
                let endpoints = endpoints.clone();
                let gemini_request = gemini_request.clone();
                let model = model.clone();
                let path = path.clone();
                let claude_system_preamble = claude_system_preamble.clone();
                async move {
                    let start = Instant::now();
                    let assigned = handle
                        .get_credential(model_mask)
                        .await?
                        .ok_or(PolluxError::NoAvailableCredential)?;

                    let actor_took = start.elapsed();
                    info!(
                        channel = "antigravity",
                        lease.id = assigned.id,
                        lease.waited_us = actor_took.as_micros() as u64,
                        req.model = %model,
                        req.stream = stream,
                        req.path = %path,
                        "[Antigravity] [ID: {}] [{:?}] Post -> {}",
                        assigned.id,
                        actor_took,
                        model
                    );

                    let mut payload = AntigravityRequestMeta {
                        project: assigned.project_id.clone(),
                        request_id: Self::generate_request_id(),
                        model: model.clone(),
                    }
                    .into_request(gemini_request.clone());

                    Self::apply_claude_thinking_defaults(model.as_str(), &mut payload.request);
                    Self::backfill_function_call_ids(model.as_str(), &mut payload.request);

                    payload.prepend_system_instruction(claude_system_preamble.as_str());

                    payload
                        .request
                        .extra
                        .entry("sessionId".to_string())
                        .or_insert_with(|| Value::String(Self::generate_session_id()));

                    with_pretty_json_debug(&payload, |pretty_payload| {
                        debug!(
                            channel = "antigravity",
                            lease.id = assigned.id,
                            req.model = %model,
                            req.stream = stream,
                            req.path = %path,
                            body = %pretty_payload,
                            "[Antigravity] Prepared upstream payload"
                        );
                    });

                    let resp = post_json_with_retry(
                        "Antigravity",
                        &client,
                        endpoints.select(stream),
                        Some(Self::headers(assigned.access_token.as_str())),
                        &payload,
                    )
                    .await?;

                    if !resp.status().is_success() {
                        let status = resp.status();

                        let (action, final_error) = classify_upstream_error(
                            resp,
                            |_json: GeminiCliErrorBody| PolluxError::UpstreamStatus(status),
                            |status, _body| PolluxError::UpstreamStatus(status),
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

                        warn!(
                            lease_id = assigned.id,
                            model = %model,
                            status = %status,
                            action = ?action,
                            "[Antigravity] Upstream error"
                        );

                        return Err(final_error);
                    }
                    Ok(resp)
                }
            }
        };

        op.retry(&self.retry_policy)
            .when(|err: &PolluxError| err.is_retryable())
            .notify(|err, dur: Duration| {
                error!(
                    "[Antigravity] Upstream Error {} retry after {:?}",
                    err.to_string(),
                    dur
                );
            })
            .await
    }

    fn headers(access_token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .expect("invalid fixed auth header value"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("antigravity/1.16.5 linux/amd64"),
        );
        headers
    }

    fn request_id_from_parts(timestamp_ms: i64, request_uuid: Uuid) -> String {
        format!("{REQUEST_ID_PREFIX}/{timestamp_ms}/{request_uuid}")
    }

    fn generate_request_id() -> String {
        Self::request_id_from_parts(Utc::now().timestamp_millis(), Uuid::new_v4())
    }

    fn session_id_from_int(value: i64) -> String {
        format!("-{value}")
    }

    fn generate_session_id() -> String {
        let value = rand::rng().random_range(0..SESSION_ID_MAX_EXCLUSIVE);
        Self::session_id_from_int(value)
    }

    fn apply_claude_thinking_defaults(model: &str, request: &mut GeminiGenerateContentRequest) {
        if !model.starts_with("claude") {
            return;
        }

        let gen_config = request
            .generation_config
            .get_or_insert_with(GenerationConfig::default);

        if gen_config.thinking_config.is_none() {
            gen_config.thinking_config = Some(json!({
                "includeThoughts": true,
                "thinkingBudget": CLAUDE_THINKING_BUDGET,
            }));
        }
    }

    /// Backfill missing `id` on `functionCall` / `functionResponse` parts.
    ///
    /// Claude requires every `tool_use` block to carry a unique `id` and
    /// every `tool_result` to carry a matching `tool_use_id`.  The
    /// Antigravity upstream translates Gemini `functionCall` → Claude
    /// `tool_use` and `functionResponse` → `tool_result`, but clients
    /// omit these fields because they are not part of the standard Gemini
    /// API spec.  Generate / pair them so the upstream translation
    /// succeeds.
    fn backfill_function_call_ids(model: &str, request: &mut GeminiGenerateContentRequest) {
        if !model.starts_with("claude") {
            return;
        }

        let mut pending_call_ids: Vec<String> = Vec::new();

        for content in &mut request.contents {
            match content.role.as_deref() {
                Some("model") => {
                    pending_call_ids.clear();
                    for part in &mut content.parts {
                        let Some(obj) = part.function_call.as_mut().and_then(|v| v.as_object_mut())
                        else {
                            continue;
                        };
                        let id_val = obj.entry("id").or_insert_with(|| {
                            Value::String(format!("toolu_{}", Uuid::new_v4().simple()))
                        });
                        if let Some(id_str) = id_val.as_str() {
                            pending_call_ids.push(id_str.to_string());
                        }
                    }
                }
                Some("user") => {
                    let mut id_iter = pending_call_ids.drain(..);
                    for part in &mut content.parts {
                        let Some(obj) = part
                            .function_response
                            .as_mut()
                            .and_then(|v| v.as_object_mut())
                        else {
                            continue;
                        };
                        let Some(matching_id) = id_iter.next() else {
                            break;
                        };
                        obj.entry("id").or_insert(Value::String(matching_id));
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_id_uses_agent_timestamp_uuid_shape() {
        let id = AntigravityClient::request_id_from_parts(
            1234,
            Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap(),
        );
        assert_eq!(id, "agent/1234/00000000-0000-4000-8000-000000000000");
    }

    #[test]
    fn endpoints_use_expected_literals() {
        let endpoints = AntigravityClient::default_endpoints();
        assert_eq!(
            endpoints.select(false).as_str(),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
        assert_eq!(
            endpoints.select(true).as_str(),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn session_id_is_negative_decimal_string() {
        assert_eq!(AntigravityClient::session_id_from_int(42), "-42");
        assert_eq!(AntigravityClient::session_id_from_int(0), "-0");
    }

    #[test]
    fn claude_requests_get_default_thinking_config_when_missing() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
        }))
        .expect("request must parse");

        AntigravityClient::apply_claude_thinking_defaults(
            "claude-sonnet-4-5-thinking",
            &mut request,
        );

        assert_eq!(
            request
                .generation_config
                .as_ref()
                .and_then(|cfg| cfg.thinking_config.as_ref()),
            Some(&json!({
                "includeThoughts": true,
                "thinkingBudget": CLAUDE_THINKING_BUDGET,
            }))
        );
    }

    #[test]
    fn claude_requests_keep_existing_thinking_config() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "generationConfig": {
                "thinkingConfig": {
                    "includeThoughts": false,
                    "thinkingBudget": 2048
                }
            }
        }))
        .expect("request must parse");

        AntigravityClient::apply_claude_thinking_defaults(
            "claude-sonnet-4-5-thinking",
            &mut request,
        );

        assert_eq!(
            request
                .generation_config
                .as_ref()
                .and_then(|cfg| cfg.thinking_config.as_ref()),
            Some(&json!({
                "includeThoughts": false,
                "thinkingBudget": 2048
            }))
        );
    }

    #[test]
    fn non_claude_requests_do_not_get_thinking_config_default() {
        let request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
        }))
        .expect("request must parse");

        let model = "gemini-2.5-pro";
        let mut request = request;
        AntigravityClient::apply_claude_thinking_defaults(model, &mut request);

        assert!(request.generation_config.is_none());
    }

    #[test]
    fn backfill_injects_id_into_function_call_without_one() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Berlin"}
                        }
                    }]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("claude-opus-4-6-thinking", &mut request);

        let fc = request.contents[0].parts[0].function_call.as_ref().unwrap();
        let id = fc.get("id").expect("id must be injected");
        assert!(id.as_str().unwrap().starts_with("toolu_"));
    }

    #[test]
    fn backfill_preserves_existing_id() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Berlin"},
                            "id": "toolu_original_id"
                        }
                    }]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("claude-opus-4-6-thinking", &mut request);

        let fc = request.contents[0].parts[0].function_call.as_ref().unwrap();
        assert_eq!(fc.get("id").unwrap().as_str().unwrap(), "toolu_original_id");
    }

    #[test]
    fn backfill_skips_non_claude_models() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Berlin"}
                        }
                    }]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("gemini-2.5-pro", &mut request);

        let fc = request.contents[0].parts[0].function_call.as_ref().unwrap();
        assert!(fc.get("id").is_none());
    }

    #[test]
    fn backfill_skips_user_content_without_preceding_model() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": "get_weather",
                            "response": {"temp": 15}
                        }
                    }]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("claude-opus-4-6-thinking", &mut request);

        let fr = request.contents[0].parts[0]
            .function_response
            .as_ref()
            .unwrap();
        // No preceding model functionCall, so no id injected
        assert!(fr.get("id").is_none());
    }

    #[test]
    fn backfill_pairs_function_response_with_function_call_id() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [
                        {
                            "functionCall": {
                                "name": "search_web",
                                "args": {"query": "hello"}
                            }
                        },
                        {
                            "functionCall": {
                                "name": "search_web",
                                "args": {"query": "world"}
                            }
                        }
                    ]
                },
                {
                    "role": "user",
                    "parts": [
                        {
                            "functionResponse": {
                                "name": "search_web",
                                "response": {"result": "r1"}
                            }
                        },
                        {
                            "functionResponse": {
                                "name": "search_web",
                                "response": {"result": "r2"}
                            }
                        }
                    ]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("claude-opus-4-6-thinking", &mut request);

        // functionCall parts should have ids
        let fc0_id = request.contents[0].parts[0]
            .function_call
            .as_ref()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let fc1_id = request.contents[0].parts[1]
            .function_call
            .as_ref()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();

        // functionResponse parts should have matching ids
        let fr0_id = request.contents[1].parts[0]
            .function_response
            .as_ref()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap();
        let fr1_id = request.contents[1].parts[1]
            .function_response
            .as_ref()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap();

        assert_eq!(fr0_id, fc0_id);
        assert_eq!(fr1_id, fc1_id);
    }

    #[test]
    fn backfill_preserves_existing_function_response_id() {
        let mut request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Berlin"},
                            "id": "toolu_call_001"
                        }
                    }]
                },
                {
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": "get_weather",
                            "response": {"temp": 15},
                            "id": "toolu_existing_response_id"
                        }
                    }]
                }
            ]
        }))
        .expect("request must parse");

        AntigravityClient::backfill_function_call_ids("claude-opus-4-6-thinking", &mut request);

        let fr = request.contents[1].parts[0]
            .function_response
            .as_ref()
            .unwrap();
        assert_eq!(
            fr.get("id").unwrap().as_str().unwrap(),
            "toolu_existing_response_id"
        );
    }
}
