pub mod client;
mod errors;
mod identity;
mod manager;
mod model_mask;
pub(crate) mod oauth;
mod resource;
mod submission;
mod workers;

use workers::{
    CodexOauthWorkerHandle, CredentialJob, CredentialJobKind, CredentialProcessError,
    CredentialProcessResult,
};

pub use manager::CodexActorHandle;
pub(in crate::providers) use manager::spawn;
pub(crate) use model_mask::{SUPPORTED_MODEL_MASK, SUPPORTED_MODEL_NAMES, model_mask};
pub(crate) use submission::CodexRefreshTokenSeed;

/// Hard-coded Codex-style User-Agent string kept as a fallback.
///
/// This is intentionally fixed (no runtime detection) to keep behavior predictable.
pub(crate) const CODEX_USER_AGENT: &str =
    "codex-tui/0.125.0 (Debian 12.0.0; x86_64) vscode/1.117.0 (codex-tui; 0.125.0)";

pub(crate) const DEFAULT_ORIGINATOR: &str = "codex-tui";
