use serde::{Deserialize, Serialize};
use serde_json;
use std::sync::LazyLock;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModel {
    name: String,
    version: Option<String>,
    display_name: String,
    description: Option<String>,
    input_token_limit: Option<u64>,
    output_token_limit: Option<u64>,
    supported_generation_methods: Option<Vec<String>>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u64>,
    max_temperature: Option<f64>,
    thinking: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GeminiModelList {
    models: Vec<GeminiModel>,
}

/// OpenAI-compatible model entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAIModel {
    pub id: String,
    pub object: String,
    pub owned_by: String,
    pub display_name: String,
}

/// OpenAI-compatible model list wrapper.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAIModelList {
    pub object: String,
    pub data: Vec<OpenAIModel>,
}

/// Embedded Gemini native models response (matches Google Generative Language models endpoint).
pub const GEMINI_NATIVE_MODELS_JSON: &str = r#"{
  "models": [
    {
      "name": "models/gemini-2.5-flash",
      "version": "001",
      "displayName": "Gemini 2.5 Flash",
      "description": "Stable version of Gemini 2.5 Flash, our mid-size multimodal model that supports up to 1 million tokens, released in June of 2025.",
      "inputTokenLimit": 1048576,
      "outputTokenLimit": 65536,
      "supportedGenerationMethods": [
        "generateContent",
        "countTokens",
        "createCachedContent",
        "batchGenerateContent"
      ],
      "temperature": 1,
      "topP": 0.95,
      "topK": 64,
      "maxTemperature": 2,
      "thinking": true
    },
    {
      "name": "models/gemini-2.5-pro",
      "version": "2.5",
      "displayName": "Gemini 2.5 Pro",
      "description": "Stable release (June 17th, 2025) of Gemini 2.5 Pro",
      "inputTokenLimit": 1048576,
      "outputTokenLimit": 65536,
      "supportedGenerationMethods": [
        "generateContent",
        "countTokens",
        "createCachedContent",
        "batchGenerateContent"
      ],
      "temperature": 1,
      "topP": 0.95,
      "topK": 64,
      "maxTemperature": 2,
      "thinking": true
    },
    {
      "name": "models/gemini-2.5-flash-lite",
      "version": "001",
      "displayName": "Gemini 2.5 Flash-Lite",
      "description": "Stable version of Gemini 2.5 Flash-Lite, released in July of 2025",
      "inputTokenLimit": 1048576,
      "outputTokenLimit": 65536,
      "supportedGenerationMethods": [
        "generateContent",
        "countTokens",
        "createCachedContent",
        "batchGenerateContent"
      ],
      "temperature": 1,
      "topP": 0.95,
      "topK": 64,
      "maxTemperature": 2,
      "thinking": true
    },
    {
      "name": "models/gemini-3-pro-preview",
      "version": "3-pro-preview-11-2025",
      "displayName": "Gemini 3 Pro Preview",
      "description": "Gemini 3 Pro Preview",
      "inputTokenLimit": 1048576,
      "outputTokenLimit": 65536,
      "supportedGenerationMethods": [
        "generateContent",
        "countTokens",
        "createCachedContent",
        "batchGenerateContent"
      ],
      "temperature": 1,
      "topP": 0.95,
      "topK": 64,
      "maxTemperature": 2,
      "thinking": true
    }
  ]
}"#;

pub static GEMINI_NATIVE_MODELS: LazyLock<GeminiModelList> = LazyLock::new(|| {
    serde_json::from_str(GEMINI_NATIVE_MODELS_JSON).expect("embedded models JSON must be valid")
});

pub static GEMINI_OAI_MODELS: LazyLock<OpenAIModelList> = LazyLock::new(|| {
    let data = GEMINI_NATIVE_MODELS
        .models
        .iter()
        .map(|m| OpenAIModel {
            id: m.name.clone(),
            object: "model".to_string(),
            owned_by: "google".to_string(),
            display_name: m.display_name.clone(),
        })
        .collect();

    OpenAIModelList {
        object: "list".to_string(),
        data,
    }
});
