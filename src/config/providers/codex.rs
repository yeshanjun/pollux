use serde::{Deserialize, Serialize};
use url::Url;

use super::ProviderDefaults;

fn default_api_url() -> Url {
    Url::parse("https://chatgpt.com").expect("invalid fixed Codex base URL")
}

/// Codex provider configuration managed by Figment.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexConfig {
    /// Codex API base URL.
    /// TOML: `providers.codex.custom_api_url`. Default: `https://chatgpt.com`.
    #[serde(default = "default_api_url")]
    pub custom_api_url: Url,

    /// Optional upstream HTTP proxy. If set, used for reqwest clients.
    /// TOML: `providers.codex.proxy`. Example: `http://127.0.0.1:1080`.
    /// Falls back to `providers.defaults.proxy` when unset.
    #[serde(default)]
    pub proxy: Option<Url>,

    /// OAuth refresh requests per second (TPS) for the refresh worker.
    /// TOML: `providers.codex.oauth_tps`. Default: `5`.
    #[serde(default = "default_oauth_tps")]
    pub oauth_tps: usize,

    /// List of supported model names (allowlist). Each name maps to a bit in the global model
    /// catalog and corresponds to an independent credential queue.
    /// TOML: `providers.codex.model_list`.
    #[serde(default = "default_model_list")]
    pub model_list: Vec<String>,

    /// Allow HTTP/2 multiplexing for reqwest clients; disabled forces HTTP/1.
    /// TOML: `providers.codex.enable_multiplexing`.
    /// Falls back to `providers.defaults.enable_multiplexing`.
    #[serde(default)]
    pub enable_multiplexing: Option<bool>,

    /// Max retry attempts for Codex upstream calls.
    /// TOML: `providers.codex.retry_max_times`.
    /// Falls back to `providers.defaults.retry_max_times`.
    #[serde(default)]
    pub retry_max_times: Option<usize>,

    /// Optional custom trace header name for upstream requests.
    /// TOML: `providers.codex.trace_header`.
    /// Falls back to `providers.defaults.trace_header`.
    #[serde(default)]
    pub trace_header: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexResolvedConfig {
    pub custom_api_url: Url,
    pub proxy: Option<Url>,
    pub oauth_tps: usize,
    pub model_list: Vec<String>,
    pub enable_multiplexing: bool,
    pub retry_max_times: usize,
    pub trace_header: Option<String>,
}

impl CodexConfig {
    pub fn resolve(&self, defaults: &ProviderDefaults) -> CodexResolvedConfig {
        CodexResolvedConfig {
            custom_api_url: self.custom_api_url.clone(),
            proxy: self.proxy.clone().or_else(|| defaults.proxy.clone()),
            oauth_tps: self.oauth_tps,
            model_list: self.model_list.clone(),
            enable_multiplexing: self
                .enable_multiplexing
                .unwrap_or(defaults.enable_multiplexing),
            retry_max_times: self.retry_max_times.unwrap_or(defaults.retry_max_times),
            trace_header: self
                .trace_header
                .clone()
                .or_else(|| defaults.trace_header.clone()),
        }
    }
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            custom_api_url: default_api_url(),
            proxy: None,
            oauth_tps: default_oauth_tps(),
            model_list: default_model_list(),
            enable_multiplexing: None,
            retry_max_times: None,
            trace_header: None,
        }
    }
}

fn default_oauth_tps() -> usize {
    5
}

fn default_model_list() -> Vec<String> {
    vec!["gpt-4o-mini".to_string()]
}
