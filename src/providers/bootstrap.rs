use crate::config::{CodexResolvedConfig, Config, GeminiCliResolvedConfig};
use crate::db::DbActorHandle;
use crate::providers::codex::CodexActorHandle;
use crate::providers::geminicli::GeminiCliActorHandle;
use std::sync::Arc;
use tracing::info;

/// Aggregates handles for all enabled providers.
///
/// Keep this as a simple struct (vs. a dynamic registry) to preserve
/// compile-time ergonomics and avoid over-abstracting too early.
#[derive(Clone)]
pub struct Providers {
    pub geminicli: GeminiCliActorHandle,
    pub geminicli_cfg: Arc<GeminiCliResolvedConfig>,
    pub codex: CodexActorHandle,
    pub codex_cfg: Arc<CodexResolvedConfig>,
}

impl Providers {
    pub async fn spawn(db: DbActorHandle, cfg: &Config) -> Self {
        let provider_defaults = &cfg.providers.defaults;
        let geminicli_cfg = Arc::new(cfg.geminicli());
        let codex_cfg = Arc::new(cfg.codex());

        // Log resolved provider configs here so `main` stays wiring-only.
        info!(
            providers_defaults_proxy = %provider_defaults.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            providers_defaults_enable_multiplexing = provider_defaults.enable_multiplexing,
            providers_defaults_retry_max_times = provider_defaults.retry_max_times,
            "Provider defaults loaded"
        );
        info!(
            geminicli_proxy = %geminicli_cfg.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            geminicli_enable_multiplexing = geminicli_cfg.enable_multiplexing,
            geminicli_retry_max_times = geminicli_cfg.retry_max_times,
            geminicli_oauth_tps = geminicli_cfg.oauth_tps,
            geminicli_model_list = ?geminicli_cfg.model_list,
            "Gemini CLI config (effective)"
        );

        info!(
            codex_proxy = %codex_cfg.proxy.as_ref().map(|u| u.as_str()).unwrap_or("<none>"),
            codex_enable_multiplexing = codex_cfg.enable_multiplexing,
            codex_retry_max_times = codex_cfg.retry_max_times,
            codex_oauth_tps = codex_cfg.oauth_tps,
            codex_responses_url = %crate::providers::codex::CODEX_RESPONSES_URL.as_str(),
            codex_model_list = ?codex_cfg.model_list,
            "Codex config (effective)"
        );

        let geminicli = crate::providers::geminicli::spawn(db.clone(), geminicli_cfg.clone()).await;
        let codex = crate::providers::codex::spawn(db, codex_cfg.clone()).await;

        Self {
            geminicli,
            geminicli_cfg,
            codex,
            codex_cfg,
        }
    }
}
