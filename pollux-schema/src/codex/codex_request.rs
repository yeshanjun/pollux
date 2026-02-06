use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::openai::{
    OpenaiInput, OpenaiInputContent, OpenaiInputItem, OpenaiRequestBody, Reasoning,
};

/// Codex upstream request body.
///
/// We explicitly control only a small set of fields and passthrough everything else
/// via `extra` to avoid schema churn as OpenAI adds new request fields.
#[derive(Debug, Clone, Serialize)]
pub struct CodexRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,

    pub input: Vec<OpenaiInputItem>,

    pub instructions: String,

    pub model: String,

    pub parallel_tool_calls: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,

    pub store: bool,

    pub stream: bool,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl From<OpenaiRequestBody> for CodexRequestBody {
    /// Build a Codex upstream request body from an OpenAI Responses API request.
    ///
    /// Behavior:
    /// - Moves any `role=system` messages out of `input` and folds their textual content into
    ///   `instructions` (joined as blocks separated by blank lines). Non-system messages are
    ///   forwarded unchanged.
    /// - Combines explicit `instructions` (if provided) with extracted system text, dropping empty
    ///   segments.
    /// - When `reasoning` is present, ensures `include` contains `reasoning.encrypted_content` so
    ///   encrypted reasoning can be returned even when we force `store=false`.
    /// - Forces Codex-required flags: `parallel_tool_calls=true`, `stream=true`, `store=false`.
    fn from(body: OpenaiRequestBody) -> Self {
        let input = match body.input {
            Some(OpenaiInput::Items(items)) => items,
            Some(OpenaiInput::Null(())) | None => Vec::new(),
        };
        let (system_msgs, clean_input): (Vec<_>, Vec<_>) = input
            .into_iter()
            .partition(|m| m.role.as_deref() == Some("system"));

        let extracted_system_text = extract_content_from_messages(&system_msgs);

        let instructions = [body.instructions, Some(extracted_system_text)]
            .into_iter()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut include_vec = body.include.unwrap_or_default();
        if body.reasoning.is_some() {
            let key = "reasoning.encrypted_content";
            if !include_vec.iter().any(|x| x == key) {
                include_vec.push(key.to_string());
            }
        }
        let final_include = if include_vec.is_empty() {
            None
        } else {
            Some(include_vec)
        };
        Self {
            model: body.model,
            input: clean_input,
            instructions,
            include: final_include,
            reasoning: body.reasoning,
            parallel_tool_calls: true,
            stream: true,
            store: false,
            extra: body.extra,
        }
    }
}

/// Extract textual content from a list of OpenAI input messages.
///
/// - Within a single message, all text-like content parts are joined with "\n".
/// - Across messages, message blocks are joined with "\n\n".
/// - Non-text content parts are ignored; empty/whitespace-only blocks are dropped.
fn extract_content_from_messages(messages: &[OpenaiInputItem]) -> String {
    messages
        .iter()
        .filter_map(|msg| {
            let text = match msg.content.as_ref() {
                Some(OpenaiInputContent::Parts(parts)) => parts
                    .iter()
                    .filter_map(|val| match val {
                        Value::String(s) => Some(s.as_str()),
                        Value::Object(o) => o.get("text").and_then(|v| v.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<&str>>()
                    .join("\n"),
                Some(OpenaiInputContent::Null(())) | None => String::new(),
            };
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        })
        .collect::<Vec<String>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_request_body_forces_stream_true() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "stream": false,
            "store": true,
            "instructions": "hi",
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(out.get("stream"), Some(&json!(true)));
        assert_eq!(out.get("store"), Some(&json!(false)));
        assert_eq!(out.get("model"), Some(&json!("gpt-4o-mini")));
        assert_eq!(out.get("instructions"), Some(&json!("hi")));
    }

    #[test]
    fn codex_request_body_infers_instructions_from_system_input_message() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [
                {"role": "system", "content": "system-instructions"},
                {"role": "user", "content": "hi"}
            ],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        assert_eq!(codex.input.len(), 1);
        assert_eq!(codex.input[0].role, Some("user".to_string()));
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(out.get("instructions"), Some(&json!("system-instructions")));
        assert_eq!(
            out.get("input").and_then(|v| v.as_array()).map(|v| v.len()),
            Some(1)
        );
        assert_eq!(
            out.get("input")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("role"))
                .and_then(|v| v.as_str()),
            Some("user")
        );
    }

    #[test]
    fn codex_request_body_appends_system_input_messages_to_explicit_instructions() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "instructions": "explicit-instructions",
            "input": [
                {"role": "system", "content": "system-instructions"},
                {"role": "user", "content": "hi"}
            ],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        assert_eq!(codex.input.len(), 1);
        assert_eq!(codex.input[0].role, Some("user".to_string()));
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(
            out.get("instructions"),
            Some(&json!("explicit-instructions\n\nsystem-instructions"))
        );
    }

    #[test]
    fn codex_request_body_concatenates_multiple_system_input_messages() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [
                {"role": "system", "content": "sys-a"},
                {"role": "user", "content": "u1"},
                {"role": "system", "content": [
                    {"type": "input_text", "text": "sys-b1"},
                    {"type": "input_text", "text": "sys-b2"}
                ]},
                {"role": "user", "content": "u2"}
            ],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        assert_eq!(codex.input.len(), 2);
        assert_eq!(codex.input[0].role, Some("user".to_string()));
        assert_eq!(codex.input[1].role, Some("user".to_string()));
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(
            out.get("instructions"),
            Some(&json!("sys-a\n\nsys-b1\nsys-b2"))
        );
    }

    #[test]
    fn codex_request_body_inserts_empty_instructions_when_missing_everywhere() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{"role": "user", "content": "hi"}],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(out.get("instructions"), Some(&json!("")));
    }

    #[test]
    fn codex_request_body_forwards_include_to_upstream_payload() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "include": ["web_search_call.action.sources"],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(
            out.get("include"),
            Some(&json!(["web_search_call.action.sources"]))
        );
    }

    #[test]
    fn codex_request_body_adds_reasoning_encrypted_content_to_include_when_reasoning_present() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "reasoning": {"effort": "low"},
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(
            out.get("include"),
            Some(&json!(["reasoning.encrypted_content"]))
        );
        assert_eq!(out.get("reasoning"), Some(&json!({"effort": "low"})));
    }

    #[test]
    fn codex_request_body_does_not_duplicate_reasoning_encrypted_content_include() {
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [],
            "include": ["reasoning.encrypted_content", "web_search_call.action.sources"],
            "reasoning": {"effort": "low"},
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");
        assert_eq!(
            out.get("include"),
            Some(&json!([
                "reasoning.encrypted_content",
                "web_search_call.action.sources"
            ]))
        );
    }

    #[test]
    fn codex_request_body_passthroughs_encrypted_content_on_input_messages() {
        let enc = "gAAAA-test";
        let body: OpenaiRequestBody = serde_json::from_value(json!({
            "model": "gpt-4o-mini",
            "input": [{
                "role": "assistant",
                "content": null,
                "encrypted_content": enc,
            }],
        }))
        .expect("failed to deserialize");

        let codex: CodexRequestBody = body.into();
        let out = serde_json::to_value(&codex).expect("failed to serialize");

        let first = out
            .get("input")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .expect("missing input[0]");

        assert_eq!(first.get("role"), Some(&json!("assistant")));
        assert_eq!(first.get("content"), Some(&Value::Null));
        assert_eq!(first.get("encrypted_content"), Some(&json!(enc)));
    }
}
