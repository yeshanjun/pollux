use super::IsRetryable;
use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error as ThisError;

use crate::providers::{ActionForError, MappingAction, UPSTREAM_BODY_PREVIEW_CHARS};

#[derive(Debug, ThisError)]
pub enum GeminiCliError {
    #[error("Request rejected")]
    RequestRejected {
        status: StatusCode,
        body: GeminiErrorObject,
        debug_message: Option<String>,
    },

    /// No usable credential is currently available.
    #[error("No available credential")]
    NoAvailableCredential,

    /// Upstream error that matched a provider mapping rule.
    #[error("Upstream mapped error: status={status} body={body:?}")]
    UpstreamMappedError {
        status: StatusCode,
        body: GeminiCliErrorBody,
    },

    /// Upstream fallback error (rule unmatched or body unstructured).
    #[error("Upstream fallback error: status={status}, body={body:.200}")]
    UpstreamFallbackError {
        status: StatusCode,
        /// Raw upstream body is preserved for internal diagnostics/logging only.
        body: String,
    },

    /// Transport-level failure (DNS, connect, timeouts, etc).
    #[error("HTTP request error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("Stream protocol error: {0}")]
    StreamProtocolError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<JsonRejection> for GeminiCliError {
    fn from(rejection: JsonRejection) -> Self {
        let debug_message = rejection.to_string();
        match rejection {
            JsonRejection::BytesRejection(_) => GeminiCliError::RequestRejected {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                body: GeminiErrorObject::for_status(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "PAYLOAD_TOO_LARGE",
                    "request body too large",
                ),
                debug_message: Some(debug_message),
            },
            JsonRejection::JsonSyntaxError(_) => GeminiCliError::RequestRejected {
                status: StatusCode::BAD_REQUEST,
                body: GeminiErrorObject::for_status(
                    StatusCode::BAD_REQUEST,
                    "INVALID_ARGUMENT",
                    "invalid JSON",
                ),
                debug_message: Some(debug_message),
            },
            _ => GeminiCliError::RequestRejected {
                status: StatusCode::BAD_REQUEST,
                body: GeminiErrorObject::for_status(
                    StatusCode::BAD_REQUEST,
                    "INVALID_ARGUMENT",
                    "invalid request",
                ),
                debug_message: Some(debug_message),
            },
        }
    }
}

impl IntoResponse for GeminiCliError {
    fn into_response(self) -> Response {
        let (status, error_body) = match self {
            GeminiCliError::RequestRejected {
                status,
                body,
                debug_message,
            } => {
                if let Some(debug_message) = debug_message {
                    tracing::warn!(
                        status = %status,
                        code = body.code,
                        err_status = %body.status,
                        message = %body.message,
                        debug_message = %debug_message,
                        "Gemini request rejected"
                    );
                } else {
                    tracing::warn!(
                        status = %status,
                        code = body.code,
                        err_status = %body.status,
                        message = %body.message,
                        "Gemini request rejected"
                    );
                }
                (status, body)
            }

            GeminiCliError::UpstreamMappedError { status, body } => {
                let cleaned = GeminiErrorBody::from(body).inner;
                tracing::warn!(
                    status = %status,
                    code = cleaned.code,
                    err_status = %cleaned.status,
                    message = %cleaned.message,
                    "Gemini upstream mapped error"
                );
                (status, cleaned)
            }

            GeminiCliError::UpstreamFallbackError { status, body } => {
                let status_str = match status {
                    StatusCode::TOO_MANY_REQUESTS => "RESOURCE_EXHAUSTED",
                    StatusCode::UNAUTHORIZED => "UNAUTHENTICATED",
                    StatusCode::FORBIDDEN => "PERMISSION_DENIED",
                    StatusCode::NOT_FOUND => "NOT_FOUND",
                    _ => "UNKNOWN",
                };
                tracing::warn!(
                    status = %status,
                    raw_body = %format!("{:.len$}", body, len = UPSTREAM_BODY_PREVIEW_CHARS),
                    "Gemini upstream fallback error"
                );
                (
                    status,
                    GeminiErrorObject::for_status(
                        status,
                        status_str,
                        format!("Upstream returned {status}"),
                    ),
                )
            }

            GeminiCliError::NoAvailableCredential => (
                StatusCode::SERVICE_UNAVAILABLE,
                GeminiErrorObject::for_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "UNAVAILABLE",
                    "No available credentials to process the request.",
                ),
            ),

            GeminiCliError::Reqwest(e) => {
                tracing::warn!(error = %e, status = ?e.status(), "Gemini reqwest error");
                (
                    StatusCode::BAD_GATEWAY,
                    GeminiErrorObject::for_status(
                        StatusCode::BAD_GATEWAY,
                        "UNAVAILABLE",
                        "Upstream service error.",
                    ),
                )
            }

            GeminiCliError::StreamProtocolError(e) => {
                tracing::warn!(error = %e, "Gemini stream protocol error");
                (
                    StatusCode::BAD_GATEWAY,
                    GeminiErrorObject::for_status(
                        StatusCode::BAD_GATEWAY,
                        "UNAVAILABLE",
                        "Upstream stream protocol error.",
                    ),
                )
            }

            GeminiCliError::Internal(e) => {
                tracing::error!(error = %e, "Gemini internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    GeminiErrorObject::for_status(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL",
                        "An internal server error occurred.",
                    ),
                )
            }
        };

        let resp_json = GeminiErrorBody { inner: error_body };
        (status, Json(resp_json)).into_response()
    }
}

impl From<crate::PolluxError> for GeminiCliError {
    fn from(err: crate::PolluxError) -> Self {
        match err {
            crate::PolluxError::NoAvailableCredential => GeminiCliError::NoAvailableCredential,
            crate::PolluxError::ReqwestError(e) => GeminiCliError::Reqwest(e),
            crate::PolluxError::StreamProtocolError(s) => GeminiCliError::StreamProtocolError(s),
            other => GeminiCliError::Internal(other.to_string()),
        }
    }
}

impl IsRetryable for GeminiCliError {
    fn is_retryable(&self) -> bool {
        match self {
            // Transport errors are already retried inside GeminiApi.
            GeminiCliError::Reqwest(_) => false,

            GeminiCliError::UpstreamFallbackError { status, .. } => matches!(
                *status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
            ),

            GeminiCliError::UpstreamMappedError { status, .. } => matches!(
                *status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
            ),

            _ => false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GeminiErrorBody {
    #[serde(rename = "error")]
    pub inner: GeminiErrorObject,
}

#[derive(Debug, Serialize)]
pub struct GeminiErrorObject {
    pub code: u16,
    pub message: String,
    pub status: String,
}

impl GeminiErrorObject {
    pub(crate) fn for_status(
        code: StatusCode,
        status: &'static str,
        message: impl Into<String>,
    ) -> Self {
        GeminiErrorObject {
            code: code.as_u16(),
            message: message.into(),
            status: status.to_string(),
        }
    }
}

impl From<GeminiCliErrorBody> for GeminiErrorBody {
    fn from(upstream_err: GeminiCliErrorBody) -> Self {
        let GeminiCliErrorBody { inner } = upstream_err;
        let GeminiCliErrorObject {
            code,
            message,
            status,
            details: _,
            extra: _,
        } = inner;
        GeminiErrorBody {
            inner: GeminiErrorObject {
                code: code.unwrap_or(0),
                message: message.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
                    "Upstream error (check server logs for details).".to_string()
                }),
                status: status
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "UNKNOWN".to_string()),
            },
        }
    }
}

/// Gemini API error response structure.
#[derive(Debug, Deserialize, Serialize)]
pub struct GeminiCliErrorBody {
    #[serde(rename = "error")]
    pub inner: GeminiCliErrorObject,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GeminiCliErrorObject {
    /// Numeric error code returned by upstream (often equals the HTTP status code, e.g. `429`/`404`).
    ///
    /// Example:
    /// - `429` with `status="RESOURCE_EXHAUSTED"`
    /// - `404` with `status="NOT_FOUND"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,

    /// Human-readable error message from upstream.
    ///
    /// Example (`429`):
    /// - `"You have exhausted your capacity on this model. Your quota will reset after ..."`
    /// - `"No capacity available for model ..."`
    ///
    /// Example (`404`):
    /// - `"Requested entity was not found."`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Upstream status string (Google-style canonical status name).
    ///
    /// Example:
    /// - `"RESOURCE_EXHAUSTED"` (rate limit / capacity)
    /// - `"NOT_FOUND"` (model/resource not found)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    /// Structured error details array returned by upstream.
    ///
    /// This field is present on some errors (notably `429`), and may include objects like:
    /// - `google.rpc.ErrorInfo` with `metadata` (e.g. `model`, `quotaResetDelay`,
    ///   `quotaResetTimeStamp` RFC3339 timestamp)
    /// - `google.rpc.RetryInfo` with `retryDelay`
    ///
    /// We keep this as `Vec<Value>` for forward compatibility and only *optionally* extract
    /// `metadata.quotaResetTimeStamp` for cooldown calculation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<Value>>,

    /// Catch-all for any additional/unknown fields inside the upstream `error` object.
    ///
    /// This is intentionally kept to make deserialization "best effort" if upstream adds new
    /// fields, and to preserve raw information for internal logs/debugging.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Variant classification for 429 `RESOURCE_EXHAUSTED` errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitVariant {
    QuotaCooldown(u64),
    CapacityPressure,
    RiskControl,
}

impl std::fmt::Display for RateLimitVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuotaCooldown(secs) => write!(f, "quota_cooldown({secs}s)"),
            Self::CapacityPressure => f.write_str("capacity_pressure"),
            Self::RiskControl => f.write_str("risk_control"),
        }
    }
}

impl GeminiCliErrorBody {
    /// Classify the 429 `RESOURCE_EXHAUSTED` variant in a single pass over `details`.
    ///
    /// Returns one of:
    /// - [`RateLimitVariant::QuotaCooldown`] — `quotaResetTimeStamp` present,
    ///   carries the computed seconds until reset (fallback 90 s if the timestamp is in the past).
    /// - [`RateLimitVariant::CapacityPressure`] — `reason = MODEL_CAPACITY_EXHAUSTED`
    ///   or `RATE_LIMIT_EXCEEDED` and no timestamp;
    ///   upstream-wide capacity shortage, not credential-specific.
    /// - [`RateLimitVariant::RiskControl`] — no `details` or unrecognized structure,
    ///   suspected upstream risk-control.
    pub fn rate_limit_variant(&self) -> RateLimitVariant {
        let Some(details) = self.inner.details.as_deref() else {
            return RateLimitVariant::RiskControl;
        };

        if let Some(variant) = details.iter().find_map(|d| {
            let ts = d.get("metadata")?.get("quotaResetTimeStamp")?.as_str()?;
            let secs = DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| (dt.with_timezone(&Utc) - Utc::now()).num_seconds())
                .filter(|&diff| diff > 0)
                .map_or(90, |diff| diff as u64 + 1);
            Some(RateLimitVariant::QuotaCooldown(secs))
        }) {
            return variant;
        }

        details
            .iter()
            .find(|d| {
                matches!(
                    d.get("reason").and_then(Value::as_str),
                    Some("MODEL_CAPACITY_EXHAUSTED" | "RATE_LIMIT_EXCEEDED")
                )
            })
            .map_or(RateLimitVariant::RiskControl, |_| {
                RateLimitVariant::CapacityPressure
            })
    }
}

impl MappingAction for GeminiCliErrorBody {
    fn try_match_rule(&self, status: StatusCode) -> Option<ActionForError> {
        let inner_status = self.inner.status.as_deref().unwrap_or_default();

        match (status, inner_status) {
            (StatusCode::UNAUTHORIZED, "UNAUTHENTICATED") => Some(ActionForError::Invalid),

            (StatusCode::FORBIDDEN, "PERMISSION_DENIED") => Some(ActionForError::Ban),

            (StatusCode::NOT_FOUND, "NOT_FOUND") => Some(ActionForError::ModelUnsupported),

            (StatusCode::TOO_MANY_REQUESTS, "RESOURCE_EXHAUSTED") => {
                Some(match self.rate_limit_variant() {
                    RateLimitVariant::QuotaCooldown(secs) => {
                        ActionForError::RateLimit(Duration::from_secs(secs.max(1)))
                    }
                    RateLimitVariant::CapacityPressure => {
                        ActionForError::RateLimit(Duration::from_secs(5))
                    }
                    RateLimitVariant::RiskControl => {
                        let secs = rand::rng().random_range(45 * 60..=60 * 60);
                        ActionForError::RateLimit(Duration::from_secs(secs))
                    }
                })
            }

            _ => None,
        }
    }

    fn action_from_status(status: StatusCode) -> ActionForError {
        match status {
            StatusCode::UNAUTHORIZED => ActionForError::Invalid,
            StatusCode::FORBIDDEN => ActionForError::None, // often WAF or transient; preserve cred
            StatusCode::NOT_FOUND => ActionForError::ModelUnsupported,
            StatusCode::TOO_MANY_REQUESTS => ActionForError::RateLimit(Duration::from_secs(60)),
            _ => ActionForError::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real upstream: quota exhausted with `quotaResetTimeStamp`.
    #[test]
    fn rate_limit_429_quota_cooldown() {
        // Use far-future timestamp to keep the test stable.
        let raw = r#"{
            "error": {
                "code": 429,
                "message": "You have exhausted your capacity on this model. Your quota will reset after 11h16m14s.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "QUOTA_EXHAUSTED",
                        "domain": "cloudcode-pa.googleapis.com",
                        "metadata": {
                            "model": "gemini-3.1-pro-preview",
                            "quotaResetDelay": "11h16m14.149781566s",
                            "quotaResetTimeStamp": "2999-01-01T00:00:00Z",
                            "uiMessage": "true"
                        }
                    },
                    {
                        "@type": "type.googleapis.com/google.rpc.RetryInfo",
                        "retryDelay": "40574.149781566s"
                    }
                ]
            }
        }"#;

        let parsed: GeminiCliErrorBody = serde_json::from_str(raw).expect("parse");
        match parsed.rate_limit_variant() {
            RateLimitVariant::QuotaCooldown(secs) => assert!(secs > 0),
            other => panic!("expected QuotaCooldown, got {other:?}"),
        }
        assert!(matches!(
            parsed.try_match_rule(StatusCode::TOO_MANY_REQUESTS),
            Some(ActionForError::RateLimit(_))
        ));
    }

    /// Real upstream: model capacity exhausted (upstream-wide, not credential-specific).
    #[test]
    fn rate_limit_429_capacity_pressure() {
        let raw = r#"{
            "error": {
                "code": 429,
                "message": "No capacity available for model gemini-3.1-pro-preview on the server",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "domain": "cloudcode-pa.googleapis.com",
                        "metadata": { "model": "gemini-3.1-pro-preview" },
                        "reason": "MODEL_CAPACITY_EXHAUSTED"
                    }
                ]
            }
        }"#;

        let parsed: GeminiCliErrorBody = serde_json::from_str(raw).expect("parse");
        assert_eq!(
            parsed.rate_limit_variant(),
            RateLimitVariant::CapacityPressure
        );
        assert_eq!(
            parsed.try_match_rule(StatusCode::TOO_MANY_REQUESTS),
            Some(ActionForError::RateLimit(Duration::from_secs(5)))
        );
    }

    /// Real upstream: bare 429 with no details — suspected risk-control.
    #[test]
    fn rate_limit_429_risk_control() {
        let raw = r#"{
            "error": {
                "code": 429,
                "message": "Resource has been exhausted (e.g. check quota).",
                "status": "RESOURCE_EXHAUSTED"
            }
        }"#;

        let parsed: GeminiCliErrorBody = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.rate_limit_variant(), RateLimitVariant::RiskControl);
        let action = parsed.try_match_rule(StatusCode::TOO_MANY_REQUESTS);
        match action {
            Some(ActionForError::RateLimit(d)) => {
                assert!(
                    d >= Duration::from_secs(45 * 60) && d <= Duration::from_secs(60 * 60),
                    "expected 45–60 min, got {d:?}"
                );
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    /// Real upstream: RATE_LIMIT_EXCEEDED with quotaResetDelay=0s and no timestamp —
    /// upstream capacity shortage, same as MODEL_CAPACITY_EXHAUSTED.
    #[test]
    fn rate_limit_429_capacity_pressure_zero_delay() {
        let raw = r#"{
            "error": {
                "code": 429,
                "message": "You have exhausted your capacity on this model.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "domain": "cloudcode-pa.googleapis.com",
                        "metadata": {
                            "model": "gemini-2.5-pro",
                            "quotaResetDelay": "0s",
                            "uiMessage": "true"
                        },
                        "reason": "RATE_LIMIT_EXCEEDED"
                    },
                    {
                        "@type": "type.googleapis.com/google.rpc.RetryInfo",
                        "retryDelay": "0s"
                    }
                ]
            }
        }"#;

        let parsed: GeminiCliErrorBody = serde_json::from_str(raw).expect("parse");
        assert_eq!(
            parsed.rate_limit_variant(),
            RateLimitVariant::CapacityPressure
        );
        assert_eq!(
            parsed.try_match_rule(StatusCode::TOO_MANY_REQUESTS),
            Some(ActionForError::RateLimit(Duration::from_secs(5)))
        );
    }

    #[test]
    fn map_404_not_found() {
        let raw = r#"{
            "error": {
                "code": 404,
                "message": "Requested entity was not found.",
                "status": "NOT_FOUND"
            }
        }"#;

        let parsed: GeminiCliErrorBody = serde_json::from_str(raw).expect("parse");
        assert_eq!(
            parsed.try_match_rule(StatusCode::NOT_FOUND),
            Some(ActionForError::ModelUnsupported)
        );
    }
}
