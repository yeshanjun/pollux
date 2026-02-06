mod codex;
mod gemini;
mod oauth;
mod pollux;

pub(crate) use codex::CodexError;
pub use gemini::{
    GeminiCliError, GeminiCliErrorBody, GeminiCliErrorObject, GeminiErrorBody, GeminiErrorObject,
};
pub use oauth::OauthError;
pub use pollux::{ApiErrorBody, ApiErrorObject, PolluxError};

pub trait IsRetryable {
    fn is_retryable(&self) -> bool;
}
