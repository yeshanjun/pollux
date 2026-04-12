use crate::server::router::PolluxState;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

pub mod extract;
pub mod handlers;
pub mod headers;
pub mod oauth;
pub mod resource;
pub mod respond;

use crate::providers::codex::SUPPORTED_MODEL_NAMES;
use pollux_schema::openai::OpenaiModelList;
use std::sync::LazyLock;

pub static CODEX_MODEL_LIST: LazyLock<OpenaiModelList> = LazyLock::new(|| {
    OpenaiModelList::from_model_names(SUPPORTED_MODEL_NAMES.iter().cloned(), "codex".to_string())
});

#[derive(Debug, Clone)]
pub struct CodexContext {
    pub model: String,
    pub stream: bool,
    pub model_mask: u64,
    /// `AHash` of `session_id`, used as a routing/cache key to pin a session to the same account.
    pub route_key: Option<u64>,
}

pub fn router() -> Router<PolluxState> {
    Router::new()
        .route(
            "/codex/v1/responses",
            post(handlers::codex_response_handler).layer(DefaultBodyLimit::max(
                crate::server::DEFAULT_API_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/codex/v1/responses/compact",
            post(handlers::codex_compact_handler).layer(DefaultBodyLimit::max(
                crate::server::DEFAULT_API_BODY_LIMIT_BYTES,
            )),
        )
        .route("/codex/v1/models", get(handlers::codex_models_handler))
        .route("/codex/resource:add", post(resource::codex_resource_add))
}
