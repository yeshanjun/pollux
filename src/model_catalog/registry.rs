use std::collections::HashMap;

/// Immutable registry of model names and indices.
/// Maintains a bidirectional mapping between `Model Name (String)` and
/// `Index (usize)`, typically initialized once at startup (e.g., via `LazyLock`).
#[derive(Debug, Clone)]
pub struct ModelRegistry {
    /// Name-to-index lookup for fast resolution during routing.
    name_to_index: HashMap<String, usize>,
    /// Index-to-name lookup for logs and diagnostics.
    index_to_name: Vec<String>,
}

impl ModelRegistry {
    /// Builds a registry from an ordered list of model names.
    /// The list order defines the model index assignment (0, 1, 2...).
    ///
    /// # Panics
    /// Panics if the number of models exceeds 64, because the bitset is `u64`.
    pub fn new(models: &[String]) -> Self {
        if models.len() > 64 {
            panic!(
                "ModelRegistry limits to 64 models (current: {}). \
                Consider upgrading to u128 or separating clusters.",
                models.len()
            );
        }

        let mut name_to_index = HashMap::with_capacity(models.len());
        let mut index_to_name = Vec::with_capacity(models.len());

        for (idx, name) in models.iter().enumerate() {
            name_to_index.insert(name.clone(), idx);
            index_to_name.push(name.clone());
        }

        Self {
            name_to_index,
            index_to_name,
        }
    }

    /// Dictionary lookup: get the index for a model name (0..63).
    ///
    /// Used by: bitmask computation (`1 << index`) and manager queue operations.
    pub fn get_index(&self, name: &str) -> Option<usize> {
        self.name_to_index.get(name).copied()
    }

    /// Reverse lookup: get the model name for an index.
    ///
    /// Used by: logging and error messages.
    pub fn get_name(&self, index: usize) -> &str {
        // Index is expected to be valid internally; fallback avoids panic.
        self.index_to_name
            .get(index)
            .map(|s| s.as_str())
            .unwrap_or("UNKNOWN_MODEL")
    }

    /// Returns the total number of models in the registry.
    ///
    /// Used by: sizing the manager queue vectors.
    pub fn len(&self) -> usize {
        self.index_to_name.len()
    }

    /// Returns true if the registry contains no models.
    pub fn is_empty(&self) -> bool {
        self.index_to_name.is_empty()
    }
}
