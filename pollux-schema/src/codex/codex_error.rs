use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Codex upstream error response schema.
#[derive(Debug, Deserialize, Serialize)]
pub struct CodexErrorBody {
    #[serde(rename = "error")]
    #[serde(default)]
    pub inner: CodexErrorObject,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CodexErrorObject {
    /// OpenAI-style error fields commonly returned by upstream services.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// OpenAI-style `type` field. Named `r#type` because `type` is a Rust keyword.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,

    /// OpenAI-style `param` field. Often a string or null.
    ///
    /// Note: `Option<Value>` does not distinguish `null` from a missing field; both deserialize as
    /// `None`. Keeping `Value` still allows non-string values if upstream ever changes types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<Value>,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CodexErrorBody {
    pub fn quota_reset_delay(&self) -> Option<u64> {
        self.inner
            .extra
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|ts| {
                let now = Utc::now().timestamp();
                let diff = ts.saturating_sub(now);
                (diff > 0).then_some((diff as u64).saturating_add(1))
            })
            .or_else(|| {
                self.inner
                    .extra
                    .get("resets_in_seconds")
                    .and_then(Value::as_u64)
                    .map(|s| s.saturating_add(1))
            })
    }

    pub fn is_unsupported_model_detail(&self) -> bool {
        let Some(detail) = self.extra.get("detail").and_then(Value::as_str) else {
            return false;
        };

        let detail_lower = detail.to_ascii_lowercase();

        detail_lower.contains("model is not supported")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_empty_body_keeps_error_envelope() {
        let body = CodexErrorBody {
            inner: CodexErrorObject::default(),
            extra: BTreeMap::new(),
        };

        let out = serde_json::to_value(&body).expect("serialize body");
        assert_eq!(out, json!({ "error": {} }));
    }

    #[test]
    fn serialize_skips_none_fields_inside_error_object() {
        let body = CodexErrorBody {
            inner: CodexErrorObject {
                message: Some("Unsupported value".to_string()),
                ..Default::default()
            },
            extra: BTreeMap::new(),
        };

        let out = serde_json::to_value(&body).expect("serialize body");
        assert_eq!(out, json!({ "error": { "message": "Unsupported value" } }));
    }

    #[test]
    fn serialize_keeps_top_level_extra_alongside_error_envelope() {
        let mut extra = BTreeMap::new();
        extra.insert("detail".to_string(), Value::String("x".to_string()));

        let body = CodexErrorBody {
            inner: CodexErrorObject::default(),
            extra,
        };

        let out = serde_json::to_value(&body).expect("serialize body");
        assert_eq!(out, json!({ "error": {}, "detail": "x" }));
    }
}
