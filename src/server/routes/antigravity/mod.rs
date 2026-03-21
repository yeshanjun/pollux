pub mod extract;
pub mod handlers;
pub mod oauth;
pub mod resource;
pub mod respond;

use crate::server::router::PolluxState;
use axum::{
    Router,
    routing::{get, post},
    extract::DefaultBodyLimit,
};

use handlers::{antigravity_models_handler, antigravity_proxy_handler};
use resource::antigravity_resource_add;

const ANTIGRAVITY_RESPONSE_BODY_LIMIT_BYTES: usize = 100 * 1024 * 1024;

pub fn router() -> Router<PolluxState> {
    Router::new()
        .route(
            "/antigravity/v1beta/models",
            get(antigravity_models_handler),
        )
        .route(
            "/antigravity/v1beta/models/{*path}",
            post(antigravity_proxy_handler),
        )
        .layer(DefaultBodyLimit::max(ANTIGRAVITY_RESPONSE_BODY_LIMIT_BYTES))
        .route("/antigravity/resource:add", post(antigravity_resource_add))
}
