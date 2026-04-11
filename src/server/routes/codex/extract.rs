use crate::error::CodexError;
use crate::providers::codex::model_mask;
use crate::utils::logging::with_pretty_json_debug;
use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Request},
    http::StatusCode,
};
use pollux_schema::OpenaiResponsesErrorObject;
use tracing::debug;

use pollux_schema::OpenaiRequestBody;

use super::CodexContext;
use super::headers::OpenaiRequestHeaders;

pub(crate) struct CodexPreprocess {
    pub body: OpenaiRequestBody,
    pub ctx: CodexContext,
    pub headers: OpenaiRequestHeaders,
}

impl<S> FromRequest<S> for CodexPreprocess
where
    S: Send + Sync,
{
    type Rejection = CodexError;

    /// Extract and validate a Codex `/codex/v1/responses` request.
    ///
    /// Responsibilities:
    /// - Deserialize the HTTP JSON body into `OpenaiRequestBody`.
    /// - Compute `model_mask` (capability bit) used for credential selection/routing.
    ///
    /// Error handling:
    /// - JSON syntax/schema errors from the `axum::Json` extractor are converted into `CodexError`
    ///   via `From<JsonRejection> for CodexError`, which emits our standardized OpenAI-style error
    ///   response body and logs the underlying parser error to `debug_message`.
    /// - Missing/empty `model` => `INVALID_MODEL`.
    /// - Model not present in this deployment's configured model set => `UNSUPPORTED_MODEL`.
    ///
    /// Notes:
    /// - We intentionally do not `trim()` or otherwise normalize `model`; matching is exact.
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Split request so we can extract headers from Parts, then reassemble for Json.
        let (mut parts, body) = req.into_parts();
        debug!(raw_headers = ?parts.headers, "[Codex] Incoming raw request headers");

        // Rejection = Infallible, so unwrap is safe.
        let codex_headers = OpenaiRequestHeaders::from_request_parts(&mut parts, state)
            .await
            .unwrap();

        let req = Request::from_parts(parts, body);
        let Json(body) = Json::<OpenaiRequestBody>::from_request(req, state).await?;

        let model = body.model.as_str();
        if model.is_empty() {
            return Err(CodexError::RequestRejected {
                status: StatusCode::BAD_REQUEST,
                body: OpenaiResponsesErrorObject {
                    code: Some("INVALID_MODEL".to_string()),
                    message: "missing or empty model".to_string(),
                    r#type: "INVALID_MODEL".to_string(),
                    param: None,
                },
                debug_message: None,
            });
        }

        let stream = body.stream;

        let Some(model_mask) = model_mask(model) else {
            return Err(CodexError::RequestRejected {
                status: StatusCode::BAD_REQUEST,
                body: OpenaiResponsesErrorObject {
                    code: Some("UNSUPPORTED_MODEL".to_string()),
                    message: "unsupported model (exact match required)".to_string(),
                    r#type: "UNSUPPORTED_MODEL".to_string(),
                    param: None,
                },
                debug_message: None,
            });
        };

        with_pretty_json_debug(&body, |pretty_body| {
            debug!(
                channel = "codex",
                req.model = %model,
                req.stream = stream,
                body = %pretty_body,
                "[Codex] Extracted normalized request body"
            );
        });

        let route_key = {
            use std::hash::Hasher;
            let mut hasher = ahash::AHasher::default();
            hasher.write(codex_headers.session_id.as_bytes());
            hasher.finish()
        };

        let ctx = CodexContext {
            model: body.model.clone(),
            stream,
            model_mask,
            route_key: Some(route_key),
        };

        Ok(Self {
            body,
            ctx,
            headers: codex_headers,
        })
    }
}
