use crate::server::router::PolluxState;
use axum::extract::rejection::JsonRejection;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
pub struct CodexResourceSeed {
    /// Only this field is used; all other fields are ignored.
    ///
    /// Aliases support common naming across other tools.
    #[serde(alias = "refreshToken")]
    pub refresh_token: Option<String>,
}

/// POST /codex/resource:add
///
/// 0-trust credential ingestion. This endpoint is intentionally a black box:
/// - It accepts a wide shape for easier migration, but only uses `refresh_token`.
/// - It returns 400 for invalid payload shapes (non-array).
/// - It returns 202 + "Success" once accepted, regardless of internal validation outcomes.
/// - Detailed outcomes are only recorded in local logs.
pub async fn codex_resource_add(
    State(state): State<PolluxState>,
    payload: Result<Json<Vec<CodexResourceSeed>>, JsonRejection>,
) -> axum::response::Response {
    let Ok(Json(seeds)) = payload else {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid payload format: The request body must be a JSON array. For example: [{\"refresh_token\":\"...\"}]",
        )
            .into_response();
    };

    let mut seen: HashSet<String> = HashSet::new();
    let refresh_tokens: Vec<String> = seeds
        .into_iter()
        .filter_map(|s| s.refresh_token)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        // Deduplicate within this request to avoid redundant refresh work.
        .filter(|t| seen.insert(t.clone()))
        .collect();

    state.providers.codex.submit_refresh_tokens(refresh_tokens);
    (StatusCode::ACCEPTED, "Success").into_response()
}
