use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GeminiModelList {
    pub models: Vec<GeminiModel>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiModel {
    pub name: String,
    pub version: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub input_token_limit: Option<u64>,
    pub output_token_limit: Option<u64>,
    pub supported_generation_methods: Option<Vec<String>>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub max_temperature: Option<f64>,
    pub thinking: Option<bool>,
}

impl GeminiModelList {
    pub fn from_model_names<I, S>(model_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let models_list = model_names
            .into_iter()
            .map(|model| {
                let name = model.into();
                GeminiModel {
                    name: name.clone(),
                    display_name: name,
                    ..Default::default()
                }
            })
            .collect();
        Self {
            models: models_list,
        }
    }
}
