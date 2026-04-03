pub mod client;
mod context;
mod manager;
mod model_mask;
mod resource;
mod thoughtsig;
mod workers;

pub use context::GeminiContext;
pub use manager::GeminiCliActorHandle;
pub(in crate::providers) use manager::spawn;
pub(crate) use model_mask::{SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES, model_mask};
pub use thoughtsig::GeminiThoughtSigService;

use crate::config::CONFIG;
use oauth2::{RedirectUrl, Scope};
use std::sync::LazyLock;

/// Gemini CLI version to impersonate.
pub(crate) const GEMINICLI_VERSION: &str = "0.36.0";

/// Default (fallback) Gemini CLI User-Agent used when the model is not yet known
/// (e.g. for the shared `reqwest::Client` default headers).
pub(crate) const GEMINICLI_USER_AGENT: &str =
    "GeminiCLI/0.36.0/gemini-2.5-pro (linux; x64; terminal)";

/// Build a per-request User-Agent that matches the original Gemini CLI format:
/// `GeminiCLI/{version}/{model} ({platform}; {arch}; {surface})`
pub(crate) fn geminicli_user_agent(model: &str) -> String {
    format!("GeminiCLI/{GEMINICLI_VERSION}/{model} (linux; x64; terminal)")
}

/// User-Agent sent by `google-auth-library` (via `DefaultTransporter`) during
/// OAuth token refresh in the original Gemini CLI.
pub(crate) const GOOGLE_AUTH_LIB_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";

/// Fixed Google OAuth endpoints used by Gemini CLI.
const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Fixed Cloud Code Gemini endpoints used by Gemini CLI.
const LOAD_CODE_ASSIST_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ONBOARD_CODE_ASSIST_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:onboardUser";

static OAUTH_CALLBACK_URL: LazyLock<RedirectUrl> = LazyLock::new(|| {
    RedirectUrl::new(format!(
        "http://localhost:{}/oauth2callback",
        CONFIG.basic.listen_port
    ))
    .expect("valid OAuth callback URL bound to localhost with configured port")
});

static GEMINICLI_SCOPES: LazyLock<Vec<Scope>> = LazyLock::new(|| {
    vec![
        Scope::new("https://www.googleapis.com/auth/cloud-platform".to_string()),
        Scope::new("https://www.googleapis.com/auth/userinfo.email".to_string()),
        Scope::new("https://www.googleapis.com/auth/userinfo.profile".to_string()),
    ]
});
