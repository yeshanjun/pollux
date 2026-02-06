use crate::providers::{ActionForError, MappingAction};
use pollux_schema::CodexErrorBody;
use reqwest::StatusCode;
use std::time::Duration;

impl MappingAction for CodexErrorBody {
    fn try_match_rule(&self, status: StatusCode) -> Option<ActionForError> {
        match (status, self) {
            // 400: detail-only unsupported model error from codex+chatgpt account path.
            (StatusCode::BAD_REQUEST, body) if body.is_unsupported_model_detail() => {
                Some(ActionForError::ModelUnsupported)
            }

            // 402: workspace deactivated or subscription expired.
            (StatusCode::PAYMENT_REQUIRED, body)
                if body.inner.r#type.as_deref() == Some("deactivated_workspace") =>
            {
                Some(ActionForError::Ban)
            }

            // 429: quota exhausted; use provided reset delay or default to 10 minutes.
            (StatusCode::TOO_MANY_REQUESTS, body)
                if body.inner.r#type.as_deref() == Some("usage_limit_reached") =>
            {
                Some(ActionForError::RateLimit(Duration::from_secs(
                    self.quota_reset_delay().unwrap_or(10 * 60).max(1),
                )))
            }

            // 429: free plan could not use codex.
            (StatusCode::TOO_MANY_REQUESTS, body)
                if body.inner.r#type.as_deref() == Some("usage_not_included") =>
            {
                Some(ActionForError::Ban)
            }

            _ => None,
        }
    }

    fn action_from_status(status: StatusCode) -> ActionForError {
        match status {
            StatusCode::UNAUTHORIZED => ActionForError::Invalid,
            StatusCode::PAYMENT_REQUIRED => ActionForError::Ban,
            // Possibly ban with WAF
            StatusCode::FORBIDDEN => ActionForError::None,
            // Immediate rate limit without structured error
            StatusCode::TOO_MANY_REQUESTS => {
                ActionForError::RateLimit(Duration::from_secs(10 * 60))
            }
            _ => ActionForError::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_match_rule_returns_none_when_type_unknown() {
        let raw = r#"{"error":{"type":"some_new_error_type"}}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(parsed.try_match_rule(StatusCode::UNAUTHORIZED), None);
        assert_eq!(parsed.try_match_rule(StatusCode::NOT_FOUND), None);
    }

    #[test]
    fn try_match_rule_returns_none_when_type_known_but_status_mismatch() {
        let raw = r#"{"error":{"type":"usage_not_included"}}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(parsed.try_match_rule(StatusCode::UNAUTHORIZED), None);
    }

    #[test]
    fn try_match_rule_prefers_structured_quota_delay_when_available() {
        let raw = r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":42}}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(
            parsed.try_match_rule(StatusCode::TOO_MANY_REQUESTS),
            Some(ActionForError::RateLimit(Duration::from_secs(43)))
        );
    }

    #[test]
    fn try_match_rule_matches_unsupported_model_detail_from_top_level_extra() {
        let raw = r#"{"detail":"The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account."}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(
            parsed.try_match_rule(StatusCode::BAD_REQUEST),
            Some(ActionForError::ModelUnsupported)
        );
    }

    #[test]
    fn try_match_rule_returns_none_for_non_matching_detail() {
        let raw = r#"{"detail":"request payload invalid"}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(parsed.try_match_rule(StatusCode::BAD_REQUEST), None);
    }

    #[test]
    fn parse_detail_only_payload_into_extra_fields() {
        let raw = r#"{"detail":"x"}"#;
        let parsed = serde_json::from_str::<CodexErrorBody>(raw).expect("parse sample");

        assert_eq!(
            parsed
                .extra
                .get("detail")
                .and_then(serde_json::Value::as_str),
            Some("x")
        );
        assert!(parsed.inner.r#type.is_none());
    }
}
