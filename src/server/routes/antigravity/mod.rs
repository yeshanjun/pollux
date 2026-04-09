pub mod extract;
pub mod handlers;
pub mod oauth;
pub mod resource;
pub mod respond;

use crate::server::router::PolluxState;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use handlers::{antigravity_models_handler, antigravity_proxy_handler};
use resource::antigravity_resource_add;

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
        .layer(DefaultBodyLimit::max(
            crate::server::DEFAULT_API_BODY_LIMIT_BYTES,
        ))
        .route("/antigravity/resource:add", post(antigravity_resource_add))
}
