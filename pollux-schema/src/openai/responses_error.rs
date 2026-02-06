//! OpenAI Responses API error schema.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codex::{CodexErrorBody, CodexErrorObject};

/// OpenAI-compatible error response schema.
///
/// Standard envelope:
/// `{ "error": { "message": "...", "type": "...", "code": "...", "param": ... } }`
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenaiResponsesErrorBody {
    #[serde(rename = "error")]
    pub inner: OpenaiResponsesErrorObject,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenaiResponsesErrorObject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<Value>,
}

impl From<CodexErrorBody> for OpenaiResponsesErrorBody {
    fn from(upstream_err: CodexErrorBody) -> Self {
        let CodexErrorBody { inner, .. } = upstream_err;
        let CodexErrorObject {
            code,
            message,
            r#type,
            param,
            ..
        } = inner;

        OpenaiResponsesErrorBody {
            inner: OpenaiResponsesErrorObject {
                code,
                message: message
                    .unwrap_or("Upstream error (check server logs for details).".to_string()),
                r#type: r#type.unwrap_or("UNKNOWN".to_string()),
                param,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    #[test]
    fn codex_error_examples_parse_and_convert() {
        let e0 = CodexErrorBody {
            inner: CodexErrorObject {
                code: Some("unsupported_value".to_string()),
                message: Some("Unsupported value".to_string()),
                r#type: Some("invalid_request_error".to_string()),
                param: Some(Value::String("reasoning.effort".to_string())),
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        };
        let out0 = OpenaiResponsesErrorBody::from(e0).inner;
        assert_eq!(out0.r#type, "invalid_request_error");
        assert_eq!(out0.code.as_deref(), Some("unsupported_value"));
        assert_eq!(
            out0.param.as_ref().and_then(Value::as_str),
            Some("reasoning.effort")
        );

        let e1 = CodexErrorBody {
            inner: CodexErrorObject {
                code: Some("invalid_encrypted_content".to_string()),
                message: Some("The encrypted content could not be verified.".to_string()),
                r#type: Some("invalid_request_error".to_string()),
                param: None,
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        };
        let out1 = OpenaiResponsesErrorBody::from(e1).inner;
        assert_eq!(out1.r#type, "invalid_request_error");
        assert_eq!(out1.code.as_deref(), Some("invalid_encrypted_content"));
        assert!(out1.param.is_none());

        let mut extra = BTreeMap::new();
        extra.insert("plan_type".to_string(), Value::String("team".to_string()));
        extra.insert("resets_at".to_string(), Value::from(1769856911_i64));
        extra.insert("resets_in_seconds".to_string(), Value::from(2198_u64));
        let e2 = CodexErrorBody {
            inner: CodexErrorObject {
                code: None,
                message: Some("The usage limit has been reached".to_string()),
                r#type: Some("usage_limit_reached".to_string()),
                param: None,
                extra,
            },
            extra: BTreeMap::new(),
        };
        assert_eq!(e2.inner.r#type.as_deref(), Some("usage_limit_reached"));
        assert!(e2.inner.extra.contains_key("plan_type"));
        assert!(e2.inner.extra.contains_key("resets_at"));
        assert!(e2.inner.extra.contains_key("resets_in_seconds"));
        let out2 = OpenaiResponsesErrorBody::from(e2).inner;
        assert_eq!(out2.r#type, "usage_limit_reached");
        assert!(out2.code.is_none());
    }

    #[test]
    fn quota_reset_delay_prefers_resets_at() {
        let now = Utc::now().timestamp();
        let resets_at = now + 10;
        let body = format!(
            "{{\"error\":{{\"type\":\"usage_limit_reached\",\"resets_at\":{resets_at},\"resets_in_seconds\":9999}}}}"
        );
        let err = serde_json::from_str::<CodexErrorBody>(&body).expect("parse");
        let got = err.quota_reset_delay().expect("delay");
        assert!(got > 0 && got <= 11, "expected 1..=11, got {got}");
    }

    #[test]
    fn quota_reset_delay_falls_back_to_resets_in_seconds_when_missing_resets_at() {
        let body = r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":2198}}"#;
        let err = serde_json::from_str::<CodexErrorBody>(body).expect("parse");
        assert_eq!(err.quota_reset_delay(), Some(2199));
    }

    #[test]
    fn quota_reset_delay_falls_back_to_resets_in_seconds_when_resets_at_in_past() {
        let now = Utc::now().timestamp();
        let resets_at = now - 10;
        let body = format!(
            "{{\"error\":{{\"type\":\"usage_limit_reached\",\"resets_at\":{resets_at},\"resets_in_seconds\":123}}}}"
        );
        let err = serde_json::from_str::<CodexErrorBody>(&body).expect("parse");
        assert_eq!(err.quota_reset_delay(), Some(124));
    }
}
