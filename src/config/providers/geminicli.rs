use serde::{Deserialize, Serialize};
use url::Url;

use super::ProviderDefaults;

/// Gemini CLI provider configuration managed by Figment.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GeminiCliConfig {
    /// Optional upstream HTTP proxy. If set, used for reqwest clients.
    /// TOML: `providers.geminicli.proxy`. Example: `http://127.0.0.1:1080`.
    /// Falls back to `providers.proxy` when unset.
    #[serde(default)]
    pub proxy: Option<Url>,

    /// OAuth refresh requests per second (TPS) for the refresh worker.
    /// TOML: `providers.geminicli.oauth_tps`. Default: `5`.
    #[serde(default = "default_oauth_tps")]
    pub oauth_tps: usize,

    /// List of supported model names. Each name corresponds to a distinct credential queue.
    /// TOML: `providers.geminicli.model_list`.
    #[serde(default = "default_model_list")]
    pub model_list: Vec<String>,

    /// Allow HTTP/2 multiplexing for reqwest clients; disabled forces HTTP/1.
    /// TOML: `providers.geminicli.enable_multiplexing`.
    /// Falls back to `providers.defaults.enable_multiplexing`.
    #[serde(default)]
    pub enable_multiplexing: Option<bool>,

    /// Max retry attempts for Gemini CLI upstream calls.
    /// TOML: `providers.geminicli.retry_max_times`.
    /// Falls back to `providers.defaults.retry_max_times`.
    #[serde(default)]
    pub retry_max_times: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct GeminiCliResolvedConfig {
    pub proxy: Option<Url>,
    pub oauth_tps: usize,
    pub model_list: Vec<String>,
    pub enable_multiplexing: bool,
    pub retry_max_times: usize,
}

impl GeminiCliConfig {
    pub fn resolve(&self, defaults: &ProviderDefaults) -> GeminiCliResolvedConfig {
        GeminiCliResolvedConfig {
            proxy: self.proxy.clone().or_else(|| defaults.proxy.clone()),
            oauth_tps: self.oauth_tps,
            model_list: self.model_list.clone(),
            enable_multiplexing: self
                .enable_multiplexing
                .unwrap_or(defaults.enable_multiplexing),
            retry_max_times: self.retry_max_times.unwrap_or(defaults.retry_max_times),
        }
    }
}

impl Default for GeminiCliConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            oauth_tps: default_oauth_tps(),
            model_list: default_model_list(),
            enable_multiplexing: None,
            retry_max_times: None,
        }
    }
}

fn default_oauth_tps() -> usize {
    5
}

fn default_model_list() -> Vec<String> {
    vec!["gemini-2.5-pro".to_string()]
}
