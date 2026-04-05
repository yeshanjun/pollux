use super::{
    extract::GeminiPreprocess,
    respond::{build_json_response, build_stream_response},
};
use crate::error::GeminiCliError;
use crate::server::router::PolluxState;
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use pollux_schema::{gemini::GeminiModelList, openai::OpenaiModelList};

pub async fn gemini_cli_handler(
    State(state): State<PolluxState>,
    GeminiPreprocess(body, ctx): GeminiPreprocess,
) -> Result<Response, GeminiCliError> {
    let upstream_resp = state
        .geminicli_caller
        .call_gemini_cli(&state.providers.geminicli, &ctx, &body)
        .await?;

    if ctx.stream {
        Ok(build_stream_response(upstream_resp, state.clone()).into_response())
    } else {
        Ok(build_json_response(upstream_resp, &state)
            .await
            .into_response())
    }
}

/// Fetch Gemini native model list via API key and proxy through Pollux.
pub async fn gemini_models_handler() -> Result<Json<GeminiModelList>, GeminiCliError> {
    Ok(Json((super::GEMINI_MODEL_LIST).clone()))
}

/// Fetch Gemini models in OpenAI-compatible list format.
pub async fn gemini_openai_models_handler() -> Result<Json<OpenaiModelList>, GeminiCliError> {
    Ok(Json((super::GEMINI_OPENAI_MODEL_LIST).clone()))
}
