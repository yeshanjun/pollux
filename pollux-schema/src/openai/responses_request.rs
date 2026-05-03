//! OpenAI Responses API request schema.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

crate::impl_string_or_array_opt_deserializer!(
    deserialize_openai_input,
    OpenaiInput,
    OpenaiInput::Items,
    |s: String| {
        OpenaiInput::Items(vec![OpenaiInputItem {
            role: Some(OpenaiRole::User),
            content: Some(OpenaiInputContent::Parts(vec![serde_json::json!({
                "type": "input_text",
                "text": s
            })])),
            extra: std::collections::BTreeMap::new(),
        }])
    }
);

crate::impl_string_or_array_opt_deserializer!(
    deserialize_openai_message_content,
    OpenaiInputContent,
    OpenaiInputContent::Parts,
    |s: String| {
        OpenaiInputContent::Parts(vec![serde_json::json!({
            "type": "input_text",
            "text": s
        })])
    }
);

/// OpenAI Responses API request body for `POST /v1/responses` ("Create a response").
///
/// Schema reference:
/// https://platform.openai.com/docs/api-reference/responses/create
///
/// Notes:
/// - The public API marks many fields as optional; Pollux may still enforce additional
///   constraints (e.g. requiring `model`) for routing/credential selection.
/// - `extra` collects unknown/new fields so deserialization doesn't break when OpenAI
///   extends the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenaiRequestBody {
    /// OpenAI docs: `array`, optional.
    ///
    /// Specify additional output data to include in the model response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,

    /// OpenAI docs: `string | array`, optional.
    ///
    /// We normalize `input` to the canonical "input items" array form:
    /// - missing => `None`
    /// - null => `None`
    /// - string => `Some(OpenaiInput::Items([{"role":"user","content":[{"type":"input_text","text":"..."}]}]))`
    /// - array => `Some(OpenaiInput::Items(...))` (passthrough, but parsed into typed input items)
    #[serde(
        default,
        deserialize_with = "deserialize_openai_input",
        skip_serializing_if = "Option::is_none"
    )]
    pub input: Option<OpenaiInput>,

    /// OpenAI docs: `string`, optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    /// OpenAI docs: `number`, optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,

    /// OpenAI docs: `string`, required.
    #[serde(default)]
    pub model: String,

    /// OpenAI docs: `boolean`, optional, default `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,

    /// OpenAI docs: `object`, optional.
    ///
    /// Controls reasoning behavior/configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,

    /// OpenAI docs: `string`, optional, default `auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// OpenAI docs: `boolean`, optional, default `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,

    /// OpenAI docs: `boolean`, optional, default `false`.
    #[serde(default)]
    pub stream: bool,

    /// OpenAI docs: `number`, optional, default `1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// OpenAI docs: `number`, optional, default `1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenaiInputItem {
    /// The OpenAI "input item" schema is not limited to message items.
    ///
    /// We keep `role` optional so the proxy can transparently passthrough non-message
    /// input items (or future schema extensions) rather than rejecting them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<OpenaiRole>,

    /// OpenAI docs: `string | array`.
    ///
    /// We normalize:
    /// - missing/null => `None`
    /// - string => `[{ "type": "input_text", "text": "..." }]`
    /// - array  => passthrough
    #[serde(
        default,
        deserialize_with = "deserialize_openai_message_content",
        skip_serializing_if = "Option::is_none"
    )]
    pub content: Option<OpenaiInputContent>,

    /// Collect unknown fields so we can passthrough new OpenAI schema fields (e.g.
    /// `encrypted_content`) without having to update our struct immediately.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Role for an OpenAI Responses message input item.
///
/// Requests with unknown roles are rejected at the schema boundary while
/// role-less non-message items can still pass through via `OpenaiInputItem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenaiRole {
    /// End-user message content.
    User,
    /// Previous model output included in conversation history.
    Assistant,
    /// System-level instructions.
    System,
    /// Developer-level instructions.
    Developer,
}

/// Normalized OpenAI Responses input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenaiInput {
    /// Canonical input item array.
    Items(Vec<OpenaiInputItem>),
}

/// Normalized content for OpenAI message input items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenaiInputContent {
    /// Canonical content parts array.
    Parts(Vec<Value>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_request_body_normalizes_string_input_to_message_item() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": "hello",
        }))
        .expect("failed to deserialize");

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: Some(OpenaiRole::User),
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hello"
                })])),
                extra: BTreeMap::new(),
            }]))
        );
    }

    #[test]
    fn openai_request_body_accepts_array_input_messages() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}],
            }],
        }))
        .expect("failed to deserialize");

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: Some(OpenaiRole::User),
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hi"
                })])),
                extra: {
                    let mut m = BTreeMap::new();
                    m.insert("type".to_string(), json!("message"));
                    m
                },
            }]))
        );
    }

    #[test]
    fn openai_request_body_accepts_string_message_content() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "role": "user",
                "content": "hi",
            }],
        }))
        .expect("failed to deserialize");

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: Some(OpenaiRole::User),
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hi"
                })])),
                extra: BTreeMap::new(),
            }]))
        );
    }

    #[test]
    fn openai_request_body_rejects_unknown_message_role() {
        let err = serde_json::from_value::<OpenaiRequestBody>(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "role": "tool",
                "content": "hi",
            }],
        }))
        .expect_err("expected deserialization to fail");

        assert_eq!(err.classify(), serde_json::error::Category::Data);
    }

    #[test]
    fn openai_request_body_accepts_non_message_input_items_without_role() {
        // Some OpenAI Responses schemas emit non-message "items" (e.g. reasoning summaries) that
        // do not have a `role`. We should transparently accept and forward them.
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "type": "reasoning",
                "summary": "auto",
                "content": null,
                "encrypted_content": "gAAAA-test",
            }],
        }))
        .expect("failed to deserialize");

        let OpenaiInput::Items(input) = body.input.expect("missing input");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0].role, None);
        assert_eq!(input[0].content, None);
        assert_eq!(input[0].extra.get("type"), Some(&json!("reasoning")));
        assert_eq!(input[0].extra.get("summary"), Some(&json!("auto")));
        assert_eq!(
            input[0].extra.get("encrypted_content"),
            Some(&json!("gAAAA-test"))
        );
    }

    #[test]
    fn openai_request_body_accepts_message_type_without_role() {
        let body = serde_json::from_value::<OpenaiRequestBody>(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "type": "message",
                "content": "hi"
            }],
        }))
        .expect("failed to deserialize");

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: None,
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hi"
                })])),
                extra: {
                    let mut m = BTreeMap::new();
                    m.insert("type".to_string(), json!("message"));
                    m
                },
            }]))
        );
    }

    #[test]
    fn openai_request_body_accepts_object_without_role_and_type() {
        let body = serde_json::from_value::<OpenaiRequestBody>(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "content": "hi"
            }],
        }))
        .expect("failed to deserialize");

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: None,
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hi"
                })])),
                extra: BTreeMap::new(),
            }]))
        );
    }

    #[test]
    fn openai_request_body_defaults_missing_input_to_none() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
        }))
        .expect("failed to deserialize");

        assert!(body.input.is_none());
    }

    #[test]
    fn openai_request_body_treats_null_input_as_none() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": null,
        }))
        .expect("failed to deserialize");

        assert_eq!(body.input, None);

        let out = serde_json::to_value(&body).expect("failed to serialize");
        assert_eq!(out.get("input"), None);
    }

    #[test]
    fn openai_request_body_treats_null_message_content_as_none() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "role": "assistant",
                "content": null,
            }],
        }))
        .expect("failed to deserialize");

        let OpenaiInput::Items(input) = body.input.expect("missing input");
        assert_eq!(input[0].content, None);

        let out = serde_json::to_value(&input[0]).expect("failed to serialize input item");
        assert_eq!(out.get("content"), None);
    }

    #[test]
    fn openai_request_body_rejects_invalid_input_type() {
        let err = serde_json::from_value::<OpenaiRequestBody>(json!({
            "model": "gpt-4o-mini",
            "input": 123,
        }))
        .expect_err("expected deserialization to fail");

        assert_eq!(err.classify(), serde_json::error::Category::Data);
    }

    #[test]
    fn openai_request_body_collects_unknown_fields_in_input_messages() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}],
                "unexpected": 1,
            }],
        }))
        .expect("failed to deserialize");

        let mut extra = BTreeMap::new();
        extra.insert("type".to_string(), json!("message"));
        extra.insert("unexpected".to_string(), json!(1));

        assert_eq!(
            body.input,
            Some(OpenaiInput::Items(vec![OpenaiInputItem {
                role: Some(OpenaiRole::User),
                content: Some(OpenaiInputContent::Parts(vec![json!({
                    "type": "input_text",
                    "text": "hi"
                })])),
                extra,
            }]))
        );
    }

    #[test]
    fn openai_request_body_collects_unknown_fields_via_flatten() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "stream": true,
            "some_new_field": {"nested": 1},
        }))
        .expect("failed to deserialize");

        assert!(body.stream);
        assert_eq!(
            body.extra.get("some_new_field"),
            Some(&json!({"nested": 1}))
        );
    }

    #[test]
    fn openai_request_body_serialization_includes_default_stream() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
        }))
        .expect("failed to deserialize");

        let out = serde_json::to_value(&body).expect("failed to serialize");
        assert_eq!(out.get("stream"), Some(&json!(false)));
    }

    #[test]
    fn openai_request_body_serialization_includes_stream_when_true() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "stream": true,
        }))
        .expect("failed to deserialize");

        let out = serde_json::to_value(&body).expect("failed to serialize");
        assert_eq!(out.get("stream"), Some(&json!(true)));
    }
}
