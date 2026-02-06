use crate::error::CodexError;
use axum::{
    Json,
    body::Bytes,
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use eventsource_stream::Eventsource;
use futures::{Stream, TryStreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio_stream::StreamExt;
use tracing::error;

const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Build SSE stream response.
pub(super) fn build_stream_response(upstream_resp: reqwest::Response) -> impl IntoResponse {
    let raw_stream = upstream_resp.bytes_stream().eventsource();
    let timed_stream =
        transform_stream(raw_stream)
            .timeout(SSE_IDLE_TIMEOUT)
            .map(|item| match item {
                Ok(Ok(event)) => Ok(event),
                Ok(Err(e)) => Err(CodexError::StreamProtocolError(e.to_string())),
                Err(_) => {
                    error!("Upstream Codex SSE stream timed out (idle > 60s)");
                    Err(CodexError::StreamProtocolError(
                        "Stream idle timeout".to_string(),
                    ))
                }
            });

    Sse::new(timed_stream).keep_alive(KeepAlive::default())
}

/// Build JSON response from a streaming upstream response.
///
/// Codex upstream can be forced into SSE mode (e.g. `stream=true`) even when the client
/// asked for a non-streaming response. In that case we buffer the SSE stream until the
/// final `response.completed` event and return the embedded `response` as JSON.
pub(super) async fn build_json_response_from_stream(
    upstream_resp: reqwest::Response,
) -> Result<(StatusCode, Json<Value>), CodexError> {
    let status = upstream_resp.status();

    let body = parse_upstream_sse_to_json(upstream_resp.bytes_stream()).await?;
    Ok((status, Json(body)))
}

async fn parse_upstream_sse_to_json<S, E>(stream: S) -> Result<Value, CodexError>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    // Codex upstream is always forced to `stream=true` (SSE). For non-stream clients we buffer the
    // SSE stream until `response.completed` and return the embedded `response` as JSON.
    let mut last_json: Option<Value> = None;

    let raw_stream = stream.eventsource();
    let timed_stream = raw_stream.timeout(SSE_IDLE_TIMEOUT);
    tokio::pin!(timed_stream);

    while let Some(item) = timed_stream.next().await {
        let upstream_event = match item {
            Ok(Ok(event)) => event,
            Ok(Err(e)) => return Err(CodexError::StreamProtocolError(e.to_string())),
            Err(_) => {
                error!("Upstream Codex stream timed out (idle > 60s)");
                return Err(CodexError::StreamProtocolError(
                    "Stream idle timeout".to_string(),
                ));
            }
        };

        if upstream_event.data.is_empty() {
            continue;
        }
        if upstream_event.data == "[DONE]" {
            break;
        }

        let value: Value = match serde_json::from_str(&upstream_event.data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if value.get("type").and_then(Value::as_str) == Some("response.completed")
            && let Some(resp) = value.get("response")
        {
            return Ok(resp.clone());
        }

        last_json = Some(value);
    }

    // Best-effort fallback: return the last JSON event we saw.
    if let Some(root) = last_json {
        if root.get("type").and_then(Value::as_str) == Some("response.completed")
            && let Some(resp) = root.get("response")
        {
            return Ok(resp.clone());
        }
        return Ok(root);
    }

    Ok(Value::Null)
}

/// Convert upstream SSE events into SSE `Event`s for clients.
pub fn transform_stream<I, E>(s: I) -> impl Stream<Item = Result<Event, E>>
where
    I: Stream<Item = Result<eventsource_stream::Event, E>>,
{
    s.try_filter_map(move |upstream_event| async move {
        if upstream_event.data.is_empty() {
            return Ok(None);
        }
        Ok(Some(Event::default().data(upstream_event.data)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use serde_json::json;

    #[tokio::test]
    async fn parse_upstream_sse_to_json_parses_response_completed_event() {
        let sse_body = concat!(
            "data: {\"type\":\"response.created\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"object\":\"response\"}}\n\n",
            "data: [DONE]\n\n",
        );

        let stream = stream::iter([Ok::<_, std::convert::Infallible>(Bytes::from_static(
            sse_body.as_bytes(),
        ))]);
        let body = parse_upstream_sse_to_json(stream).await.unwrap();
        assert_eq!(body, json!({"id":"r1","object":"response"}));
    }

    #[tokio::test]
    async fn parse_upstream_sse_to_json_handles_chunked_sse_payload() {
        let sse_body = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\"}}\n\n",
            "data: [DONE]\n\n",
        );

        let (a, b) = sse_body.split_at(10);
        let stream = stream::iter([
            Ok::<_, std::convert::Infallible>(Bytes::from_static(a.as_bytes())),
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b.as_bytes())),
        ]);
        let body = parse_upstream_sse_to_json(stream).await.unwrap();
        assert_eq!(body, json!({"id":"r2"}));
    }
}
