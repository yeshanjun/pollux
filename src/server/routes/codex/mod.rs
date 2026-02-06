use crate::server::router::PolluxState;
use axum::{
    Router,
    routing::{get, post},
};

pub mod extract;
pub mod handlers;
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
}

pub fn router() -> Router<PolluxState> {
    Router::new()
        .route(
            "/codex/v1/responses",
            post(handlers::codex_response_handler),
        )
        .route("/codex/v1/models", get(handlers::codex_models_handler))
        .route("/codex/resource:add", post(resource::codex_resource_add))
}
