use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error as ThisError;

use super::IsRetryable;
use super::oauth::OauthError;

#[derive(Debug, ThisError)]
pub enum PolluxError {
    #[error("Upstream error with status: {0}")]
    UpstreamStatus(StatusCode),

    #[error(transparent)]
    Oauth(#[from] OauthError),

    #[error("HTTP request error: {0}")]
    ReqwestError(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("URL parse error: {0}")]
    UrlError(#[from] url::ParseError),

    #[error("Stream protocol error: {0}")]
    StreamProtocolError(String),

    #[error("Missing access token; refresh first")]
    MissingAccessToken,

    #[error("Missing expiry; refresh first")]
    MissingExpiry,

    #[error("Unexpected error: {0}")]
    UnexpectedError(String),

    #[error("No available credential")]
    NoAvailableCredential,

    #[error("Ractor error: {0}")]
    RactorError(String),

    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),
}

impl PolluxError {}

impl IntoResponse for PolluxError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_body) = match self {
            PolluxError::DatabaseError(_)
            | PolluxError::RactorError(_)
            | PolluxError::UnexpectedError(_)
            | PolluxError::Oauth(OauthError::Other { .. })
            | PolluxError::IoError(_)
            | PolluxError::MissingAccessToken
            | PolluxError::MissingExpiry => {
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                let body = ApiErrorObject {
                    code: "INTERNAL_ERROR".to_string(),
                    message: "An internal server error occurred.".to_string(),
                    details: None,
                };
                (status, body)
            }

            PolluxError::Oauth(OauthError::Flow {
                code,
                message,
                details,
            }) => {
                let status = StatusCode::FORBIDDEN;
                let body = ApiErrorObject {
                    code,
                    message,
                    details,
                };
                (status, body)
            }

            PolluxError::JsonError(_) | PolluxError::Oauth(OauthError::Parse { .. }) => {
                let status = StatusCode::BAD_GATEWAY;
                let body = ApiErrorObject {
                    code: "BAD_UPSTREAM_PAYLOAD".to_string(),
                    message: "Failed to parse upstream response.".to_string(),
                    details: None,
                };
                (status, body)
            }

            PolluxError::StreamProtocolError(_)
            | PolluxError::Oauth(OauthError::Request(_))
            | PolluxError::Oauth(OauthError::ServerResponse { .. })
            | PolluxError::ReqwestError(_)
            | PolluxError::UrlError(_) => {
                let status = StatusCode::BAD_GATEWAY;
                let body = ApiErrorObject {
                    code: "UPSTREAM_ERROR".to_string(),
                    message: "Upstream service error.".to_string(),
                    details: None,
                };
                (status, body)
            }

            PolluxError::NoAvailableCredential => {
                let status = StatusCode::SERVICE_UNAVAILABLE;
                let body = ApiErrorObject {
                    code: "NO_CREDENTIAL".to_string(),
                    message: "No available credentials to process the request.".to_string(),
                    details: None,
                };
                (status, body)
            }

            PolluxError::UpstreamStatus(code)
            | PolluxError::Oauth(OauthError::UpstreamStatus(code)) => {
                let (err_code, msg) = match code {
                    StatusCode::TOO_MANY_REQUESTS => {
                        ("RATE_LIMIT", "Upstream rate limit exceeded.")
                    }
                    StatusCode::UNAUTHORIZED => ("UNAUTHORIZED", "Upstream authentication failed."),
                    StatusCode::FORBIDDEN => ("FORBIDDEN", "Upstream permission denied."),
                    StatusCode::NOT_FOUND => ("NOT_FOUND", "Upstream resource not found."),
                    _ => ("UPSTREAM_ERROR", "An upstream error occurred."),
                };
                (
                    code,
                    ApiErrorObject {
                        code: err_code.to_string(),
                        message: msg.to_string(),
                        details: None,
                    },
                )
            }
        };
        (status, Json(ApiErrorBody { inner: error_body })).into_response()
    }
}

/// Standardized API error response payload.
#[derive(Serialize)]
pub struct ApiErrorObject {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Serialize)]
pub struct ApiErrorBody {
    #[serde(rename = "error")]
    pub inner: ApiErrorObject,
}

impl IsRetryable for PolluxError {
    fn is_retryable(&self) -> bool {
        match self {
            PolluxError::ReqwestError(_) => true,
            PolluxError::UpstreamStatus(status) => matches!(
                *status,
                reqwest::StatusCode::TOO_MANY_REQUESTS
                    | reqwest::StatusCode::UNAUTHORIZED
                    | reqwest::StatusCode::FORBIDDEN
                    | reqwest::StatusCode::NOT_FOUND
            ),
            PolluxError::Oauth(OauthError::ServerResponse { .. }) => false,
            PolluxError::UnexpectedError(_) => false,
            _ => false,
        }
    }
}
