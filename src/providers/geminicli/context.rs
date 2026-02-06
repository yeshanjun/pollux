#[derive(Debug, Clone)]
pub struct GeminiContext {
    pub model: String,
    pub stream: bool,
    pub path: String,
    pub model_mask: u64,
}
