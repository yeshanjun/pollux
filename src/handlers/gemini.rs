use crate::api::gemini_client::GeminiClient;
use crate::middleware::gemini_request::GeminiPreprocess;
use crate::middleware::gemini_response::{build_json_response, build_stream_response};
use crate::types::gemini_models::{
    GEMINI_NATIVE_MODELS, GEMINI_OAI_MODELS, GeminiModelList, OpenAIModelList,
};
use crate::{NexusError, router::NexusState};
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use tracing::info;

pub async fn gemini_cli_handler(
    State(state): State<NexusState>,
    GeminiPreprocess(body, ctx): GeminiPreprocess,
) -> Result<Response, NexusError> {
    // Construct caller
    let caller = GeminiClient::new(state.client.clone());

    let upstream_resp = caller.call_gemini_cli(&state, &ctx, &body).await?;

    if ctx.stream {
        Ok(build_stream_response(upstream_resp).into_response())
    } else {
        Ok(build_json_response(upstream_resp).await.into_response())
    }
}

/// Fetch Gemini native model list via API key and proxy through Nexus.
pub async fn gemini_models_handler() -> Result<Json<GeminiModelList>, NexusError> {
    info!("Incoming request: GET /v1beta/models");
    Ok(Json((*GEMINI_NATIVE_MODELS).clone()))
}

/// Fetch Gemini models in OpenAI-compatible list format.
pub async fn openai_models_handler() -> Result<Json<OpenAIModelList>, NexusError> {
    info!("Incoming request: GET /v1beta/openai/models");
    Ok(Json((*GEMINI_OAI_MODELS).clone()))
}
