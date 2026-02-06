pub mod extract;
pub mod handlers;
pub mod oauth;
pub mod resource;
pub mod respond;

use crate::providers::geminicli::SUPPORTED_MODEL_NAMES;
use crate::server::router::PolluxState;
use handlers::{gemini_cli_handler, gemini_models_handler, gemini_openai_models_handler};
use pollux_schema::{gemini::GeminiModelList, openai::OpenaiModelList};
use resource::geminicli_resource_add;

use axum::{
    Router,
    routing::{get, post},
};
use std::sync::LazyLock;

pub static GEMINI_MODEL_LIST: LazyLock<GeminiModelList> =
    LazyLock::new(|| GeminiModelList::from_model_names(SUPPORTED_MODEL_NAMES.iter().cloned()));

pub static GEMINI_OPENAI_MODEL_LIST: LazyLock<OpenaiModelList> = LazyLock::new(|| {
    OpenaiModelList::from_model_names(
        SUPPORTED_MODEL_NAMES.iter().cloned(),
        "gemini-cli".to_string(),
    )
});

pub fn router() -> Router<PolluxState> {
    Router::new()
        .route("/geminicli/v1beta/models", get(gemini_models_handler))
        .route(
            "/geminicli/v1beta/openai/models",
            get(gemini_openai_models_handler),
        )
        .route("/geminicli/v1beta/models/{*path}", post(gemini_cli_handler))
        .route("/geminicli/resource:add", post(geminicli_resource_add))
}
