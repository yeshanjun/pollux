use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::convert::Infallible;

use crate::providers::codex::{CODEX_USER_AGENT, DEFAULT_ORIGINATOR};
use crate::providers::manifest::CodexLease;

/// Centralized header name constants — single source of truth for all custom
/// header keys used in the Codex request/response flow.
pub(crate) mod header_names {
    use axum::http::HeaderName;

    pub static SESSION_ID: HeaderName = HeaderName::from_static("session_id");
    pub static X_CLIENT_REQUEST_ID: HeaderName = HeaderName::from_static("x-client-request-id");
    pub static X_CODEX_TURN_METADATA: HeaderName = HeaderName::from_static("x-codex-turn-metadata");

    pub static ORIGINATOR: HeaderName = HeaderName::from_static("originator");
    pub static CHATGPT_ACCOUNT_ID: HeaderName = HeaderName::from_static("chatgpt-account-id");
}

/// Structured representation of the Codex CLI request headers.
///
/// Known fields are extracted into typed fields; everything else is captured
/// in `extra` as a raw `BTreeMap<String, String>` for logging/auditing.
#[derive(Debug, Clone)]
pub(crate) struct OpenaiRequestHeaders {
    pub session_id: String,
    pub turn_metadata: Option<CodexTurnMetadata>,
    /// All headers not explicitly extracted above (standard + unknown custom).
    #[allow(dead_code)]
    pub extra: BTreeMap<String, String>,
}

/// Parsed `x-codex-turn-metadata` JSON header.
///
/// `session_id` and `turn_id` are the primary fields; everything else is
/// best-effort — present when the client sends it, silently skipped otherwise.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub(crate) struct CodexTurnMetadata {
    pub session_id: Option<String>,
    #[serde(default = "generate_uuid_v4")]
    pub turn_id: String,
    #[serde(default, flatten)]
    #[allow(dead_code)]
    pub extra: BTreeMap<String, Value>,
}

fn generate_uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Headers for the upstream Codex request.
#[derive(Clone)]
pub(crate) struct CodexRequestHeaders {
    pub authorization: String,
    pub account_id: String,
    pub session_id: String,
    pub client_request_id: String,
    pub turn_metadata: Option<CodexTurnMetadata>,
}

impl std::fmt::Debug for CodexRequestHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexRequestHeaders")
            .field("authorization", &"Bearer [REDACTED]")
            .field("account_id", &self.account_id)
            .field("session_id", &self.session_id)
            .field("client_request_id", &self.client_request_id)
            .field("turn_metadata", &self.turn_metadata)
            .finish()
    }
}

impl CodexRequestHeaders {
    /// Build upstream headers from the inbound request headers and a credential lease.
    pub(crate) fn build(inbound: &OpenaiRequestHeaders, lease: &CodexLease) -> Self {
        Self {
            authorization: format!("Bearer {}", lease.access_token),
            account_id: lease.account_id.clone(),
            client_request_id: inbound.session_id.clone(),
            session_id: inbound.session_id.clone(),
            turn_metadata: inbound.turn_metadata.clone(),
        }
    }

    /// Convert into a `reqwest::header::HeaderMap` for the upstream HTTP call.
    pub(crate) fn into_header_map(self) -> HeaderMap {
        use header_names::{
            CHATGPT_ACCOUNT_ID, ORIGINATOR, SESSION_ID, X_CLIENT_REQUEST_ID, X_CODEX_TURN_METADATA,
        };

        /// Insert required headers into a `HeaderMap` in a declarative style.
        macro_rules! set_headers {
            ($map:expr, { $( $name:expr => $value:expr ),* $(,)? }) => {
                $( $map.insert(
                    $name.clone(),
                    HeaderValue::from_str(&$value)
                        .expect(concat!("invalid header value: ", stringify!($name))),
                ); )*
            };
        }

        let mut map = HeaderMap::with_capacity(6);

        set_headers!(map, {
            AUTHORIZATION        => self.authorization,
            CHATGPT_ACCOUNT_ID   => self.account_id,
            SESSION_ID           => self.session_id,
            X_CLIENT_REQUEST_ID  => self.client_request_id,
        });

        map.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(CODEX_USER_AGENT),
        );

        map.insert(
            ORIGINATOR.clone(),
            HeaderValue::from_static(DEFAULT_ORIGINATOR),
        );
        if let Some(ref meta) = self.turn_metadata
            && let Ok(json) = serde_json::to_string(meta)
            && let Ok(v) = HeaderValue::from_str(&json)
        {
            map.insert(X_CODEX_TURN_METADATA.clone(), v);
        }

        map
    }
}

impl<S: Send + Sync> FromRequestParts<S> for OpenaiRequestHeaders {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        use header_names::{SESSION_ID, X_CODEX_TURN_METADATA};

        let headers = &parts.headers;

        let get = |name: &axum::http::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string)
        };

        // session_id: single source of truth, falls back to a generated UUIDv4.
        let session_id = get(&SESSION_ID).unwrap_or_else(generate_uuid_v4);

        let mut turn_metadata = headers
            .get(&X_CODEX_TURN_METADATA)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| serde_json::from_str::<CodexTurnMetadata>(s).ok());

        // Propagate the canonical session_id into turn_metadata.
        if let Some(meta) = &mut turn_metadata {
            meta.session_id = Some(session_id.clone());
        }

        // Known header names that are extracted into typed fields above.
        let known: &[&axum::http::HeaderName] = &[&SESSION_ID, &X_CODEX_TURN_METADATA];

        // Collect everything else into `extra`.
        let extra = headers
            .iter()
            .filter(|(name, _)| !known.contains(name))
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.to_string(), v.to_string()))
            })
            .collect();

        Ok(Self {
            session_id,
            turn_metadata,
            extra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;

    /// Build a minimal `Parts` from header pairs for testing `FromRequestParts`.
    fn build_parts(headers: Vec<(&str, &str)>) -> Parts {
        let mut builder = HttpRequest::builder()
            .method("POST")
            .uri("/codex/v1/responses");
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        builder.body(()).unwrap().into_parts().0
    }

    /// Helper: extract `OpenaiRequestHeaders` from header pairs (blocking wrapper).
    fn extract_headers(headers: Vec<(&str, &str)>) -> OpenaiRequestHeaders {
        let mut parts = build_parts(headers);
        // FromRequestParts::from_request_parts is async but our impl never actually awaits,
        // so block_on is fine in tests.
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(OpenaiRequestHeaders::from_request_parts(&mut parts, &()))
            .unwrap()
    }

    #[test]
    fn extract_session_id_from_header() {
        let h = extract_headers(vec![("session_id", "sid-123")]);
        assert_eq!(h.session_id, "sid-123");
    }

    #[test]
    fn extract_generates_session_id_when_missing() {
        let h = extract_headers(vec![]);

        assert!(!h.session_id.is_empty());
        // Should be a valid UUIDv4
        assert!(uuid::Uuid::parse_str(&h.session_id).is_ok());
    }

    #[test]
    fn session_id_propagates_into_turn_metadata() {
        let meta_json = r#"{"turn_id":"tid-1","session_id":"old-sid"}"#;
        let h = extract_headers(vec![
            ("session_id", "canonical-sid"),
            ("x-codex-turn-metadata", meta_json),
        ]);

        assert_eq!(h.session_id, "canonical-sid");
        let meta = h.turn_metadata.unwrap();
        assert_eq!(meta.session_id.as_deref(), Some("canonical-sid"));
        assert_eq!(meta.turn_id, "tid-1");
    }

    #[test]
    fn upstream_header_map_contains_expected_keys() {
        let meta_json = r#"{"turn_id":"tid-2"}"#;
        let inbound = extract_headers(vec![
            ("session_id", "sid-456"),
            ("originator", "codex_cli_rs"),
            ("x-codex-turn-metadata", meta_json),
        ]);
        let lease = CodexLease {
            id: 1,
            access_token: "at-test".to_string(),
            account_id: "acct-test".to_string(),
            email: None,
        };

        let map = CodexRequestHeaders::build(&inbound, &lease).into_header_map();

        assert_eq!(map.get("authorization").unwrap(), "Bearer at-test");
        assert_eq!(map.get("chatgpt-account-id").unwrap(), "acct-test");
        assert_eq!(map.get("session_id").unwrap(), "sid-456");
        assert_eq!(map.get("originator").unwrap(), DEFAULT_ORIGINATOR);

        // x-codex-turn-metadata should be forwarded as serialized JSON.
        let meta_value = map.get("x-codex-turn-metadata").unwrap().to_str().unwrap();
        let meta: CodexTurnMetadata = serde_json::from_str(meta_value).unwrap();
        assert_eq!(meta.turn_id, "tid-2");
        assert_eq!(meta.session_id.as_deref(), Some("sid-456"));
    }
}
