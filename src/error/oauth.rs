use super::IsRetryable;
use super::pollux::PolluxError;
use axum::http::StatusCode;
use oauth2::basic::BasicErrorResponseType;
use oauth2::reqwest::Error as ReqwestClientError;
use oauth2::{HttpClientError, RequestTokenError, StandardErrorResponse};
use serde_json::Value;
use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
pub enum OauthError {
    #[error("OAuth flow error: {message}")]
    Flow {
        code: String,
        message: String,
        details: Option<Value>,
    },

    #[error("OAuth2 request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("OAuth2 upstream error with status: {0}")]
    UpstreamStatus(StatusCode),

    #[error("OAuth2 server response error: {error}")]
    ServerResponse { error: String },

    #[error("OAuth2 token endpoint parse error: {message}. Body: {body}")]
    Parse { message: String, body: String },

    #[error("OAuth2 unexpected error: {message}")]
    Other { message: String },
}

impl IsRetryable for OauthError {
    fn is_retryable(&self) -> bool {
        match self {
            OauthError::Request(_) => true,
            OauthError::UpstreamStatus(status) => {
                *status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
            }
            OauthError::Parse { .. } => true,
            _ => false,
        }
    }
}

type PkgsRequestTokenError = RequestTokenError<
    HttpClientError<ReqwestClientError>,
    StandardErrorResponse<BasicErrorResponseType>,
>;

impl From<PkgsRequestTokenError> for OauthError {
    fn from(e: PkgsRequestTokenError) -> Self {
        match e {
            RequestTokenError::ServerResponse(err) => OauthError::ServerResponse {
                error: err.error().to_string(),
            },
            RequestTokenError::Request(wrapper) => match wrapper {
                oauth2::HttpClientError::Reqwest(real_err) => OauthError::Request(*real_err),
                other => OauthError::Other {
                    message: format!("HttpClientError: {:?}", other),
                },
            },
            RequestTokenError::Parse(parse_err, body) => {
                let body_str = String::from_utf8_lossy(&body);
                let body = body_str
                    .char_indices()
                    .nth(100)
                    .map(|(idx, _)| format!("{}...<truncated>", &body_str[..idx]))
                    .unwrap_or_else(|| body_str.into_owned());
                OauthError::Parse {
                    message: parse_err.to_string(),
                    body,
                }
            }
            RequestTokenError::Other(s) => OauthError::Other { message: s },
        }
    }
}

impl From<PkgsRequestTokenError> for PolluxError {
    fn from(e: PkgsRequestTokenError) -> Self {
        OauthError::from(e).into()
    }
}
