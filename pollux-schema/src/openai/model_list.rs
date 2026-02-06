use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenaiModelList {
    pub object: String,
    pub data: Vec<OpenaiModel>,
}

impl Default for OpenaiModelList {
    fn default() -> Self {
        Self {
            object: "list".to_string(),
            data: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenaiModel {
    pub id: String,
    pub object: String,
    pub owned_by: String,
    pub display_name: String,
}

impl Default for OpenaiModel {
    fn default() -> Self {
        Self {
            id: String::new(),
            object: "model".to_string(),
            owned_by: String::new(),
            display_name: String::new(),
        }
    }
}

impl OpenaiModelList {
    pub fn from_model_names<I, S>(models_list: I, owned_by: String) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let data = models_list
            .into_iter()
            .map(|model| {
                let id = model.into();
                OpenaiModel {
                    id: id.clone(),
                    display_name: id,
                    owned_by: owned_by.clone(),
                    ..Default::default()
                }
            })
            .collect();

        Self {
            data,
            ..Default::default()
        }
    }
}
