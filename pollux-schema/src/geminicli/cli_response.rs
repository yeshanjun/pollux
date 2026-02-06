use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::gemini::{Candidate, GeminiResponseBody};

/// Generic CLI envelope wrapper.
#[derive(Debug, Deserialize)]
pub struct GeminiCliResponseBody {
    #[serde(rename = "response")]
    pub inner: GeminiCliResponseObject,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiCliResponseObject {
    #[serde(default)]
    pub candidates: Vec<Candidate>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub promptFeedback: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub usageMetadata: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modelVersion: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub responseId: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub createTime: Option<String>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl From<GeminiCliResponseBody> for GeminiResponseBody {
    fn from(body: GeminiCliResponseBody) -> Self {
        let inner = body.inner;
        GeminiResponseBody {
            candidates: inner.candidates,
            promptFeedback: inner.promptFeedback,
            usageMetadata: inner.usageMetadata,
            modelVersion: inner.modelVersion,
            responseId: inner.responseId,
            extra: inner.extra,
        }
    }
}
