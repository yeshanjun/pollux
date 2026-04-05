pub mod oauth;
#[path = "client.rs"]
pub mod upstream;

pub(crate) use upstream::GeminiClient;
