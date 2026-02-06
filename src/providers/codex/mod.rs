pub mod client;
mod errors;
mod identity;
mod manager;
mod model_mask;
pub(crate) mod oauth;
mod resource;
mod submission;
mod workers;

use std::sync::LazyLock;
use url::Url;
use workers::{CodexRefresherHandle, RefreshOutcome};

pub use manager::CodexActorHandle;
pub(in crate::providers) use manager::spawn;
pub(crate) use model_mask::{SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES, model_mask};
pub(crate) use submission::CodexRefreshTokenSeed;

pub(crate) static CODEX_RESPONSES_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://chatgpt.com/backend-api/codex/responses")
        .expect("invalid fixed Codex responses URL")
});

/// Hard-coded Codex-style User-Agent string kept as a fallback.
///
/// This is intentionally fixed (no runtime detection) to keep behavior predictable.
/// codex_cli_rs/0.95.0 (Debian 12.0.0; x86_64) vscode/1.108.2
pub(crate) const CODEX_USER_AGENT: &str =
    "codex_cli_rs/0.95.0 (Debian 12.0.0; x86_64) vscode/1.108.2";
