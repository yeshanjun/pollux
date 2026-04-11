use crate::server::router::PolluxState;
use axum::extract::rejection::JsonRejection;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
pub struct AntigravityResourceSeed {
    /// Only this field is used; all other fields are ignored.
    ///
    /// Aliases support common naming across other tools.
    #[serde(alias = "refreshToken")]
    pub refresh_token: Option<String>,
}

/// POST /antigravity/resource:add
///
/// 0-trust credential ingestion. Mirrors `/geminicli/resource:add` semantics.
pub async fn antigravity_resource_add(
    State(state): State<PolluxState>,
    payload: Result<Json<Vec<AntigravityResourceSeed>>, JsonRejection>,
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

    state
        .providers
        .antigravity
        .submit_refresh_tokens(refresh_tokens);

    (StatusCode::ACCEPTED, "Success").into_response()
}
