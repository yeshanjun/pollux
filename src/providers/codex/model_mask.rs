use crate::config::CONFIG;
use crate::model_catalog;
use std::collections::HashSet;
use std::sync::LazyLock;

pub(crate) static SUPPORTED_MODEL_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let cfg = CONFIG.codex();

    let mut seen = HashSet::<String>::new();
    cfg.model_list
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
});

pub(crate) static SUPPORTED_MODEL_MASK: LazyLock<u64> = LazyLock::new(|| {
    let mut mask = 0u64;
    for name in SUPPORTED_MODEL_NAMES.iter() {
        if let Some(bit) = model_catalog::mask(name) {
            mask |= bit;
        }
    }
    mask
});

pub(crate) fn model_mask(name: &str) -> Option<u64> {
    let bit = model_catalog::mask(name)?;
    if (*SUPPORTED_MODEL_MASK & bit) != 0 {
        Some(bit)
    } else {
        None
    }
}
