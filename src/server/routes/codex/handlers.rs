use super::{extract::CodexPreprocess, respond};
use crate::error::CodexError;
use crate::server::router::PolluxState;
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use pollux_schema::CodexRequestBody;
use pollux_schema::openai::OpenaiModelList;
use tracing::debug;

pub(super) async fn codex_response_handler(
    State(state): State<PolluxState>,
    CodexPreprocess {
        body,
        ctx,
        headers,
        route_key,
    }: CodexPreprocess,
) -> Result<Response, CodexError> {
    let codex_body: CodexRequestBody = body.into();

    debug!(
        model = %ctx.model,
        client_stream = ctx.stream,
        upstream_stream = codex_body.stream,
        model_mask = format_args!("0x{:016x}", ctx.model_mask),
        "Incoming Codex request"
    );

    let upstream_resp = state
        .codex_caller
        .call_codex(
            &state.providers.codex,
            ctx.model.as_str(),
            ctx.model_mask,
            Some(route_key),
            ctx.stream,
            &codex_body,
            &headers,
        )
        .await?;

    if ctx.stream {
        Ok(respond::build_stream_response(upstream_resp).into_response())
    } else {
        let (status, body) = respond::build_json_response_from_stream(upstream_resp).await?;
        Ok((status, body).into_response())
    }
}

pub(super) async fn codex_models_handler() -> Result<Json<OpenaiModelList>, CodexError> {
    Ok(Json(super::CODEX_MODEL_LIST.clone()))
}
