use pollux_schema::gemini::{GeminiGenerateContentRequest, Part};
use pollux_thoughtsig_core::{
    PatchEvent, PatchOutcome, Patchable, SignaturePatcher, SignaturePreview,
};
use tracing::debug;

// Minimal wrapper for `Part` due to orphan rule:
// we cannot implement `Patchable` directly on schema types
// from another crate.
struct GeminiPartPatch<'a>(&'a mut Part);

impl Patchable for GeminiPartPatch<'_> {
    fn data(&self) -> PatchEvent<'_> {
        if let Some(function_call) = self.0.function_call.as_ref() {
            return PatchEvent::FunctionCall(function_call);
        }

        if self.0.thought == Some(true) {
            if let Some(text) = self.0.text.as_deref() {
                return PatchEvent::ThoughtText(text);
            }
            return PatchEvent::ThoughtText("");
        }

        PatchEvent::None
    }

    fn thought_signature(&self) -> Option<&str> {
        self.0.thought_signature.as_deref()
    }

    fn thought_signature_mut(&mut self) -> &mut Option<String> {
        self.0.thought_signature_mut()
    }
}

/// Fill missing thought signatures on model parts (Fallback policy).
/// Parts are never removed; uncached signatures get the skip-validator dummy.
pub(super) fn patch_request(
    request: &mut GeminiGenerateContentRequest,
    patcher: &SignaturePatcher,
) {
    request
        .contents
        .iter_mut()
        .filter(|content| content.role.as_deref() == Some("model"))
        .flat_map(|content| content.parts.iter_mut())
        .for_each(|part| {
            let mut patch = GeminiPartPatch(part);
            if let PatchOutcome::Patched { cache_key } = patcher.patch(&mut patch) {
                debug!(
                    channel = "geminicli",
                    thoughtsig.phase = "fill",
                    key = ?cache_key,
                    signature = %SignaturePreview(part.thought_signature.as_deref().unwrap_or_default()),
                    "Thought signature decision"
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollux_thoughtsig_core::{CacheKeyGenerator, CacheMissPolicy, ThoughtSignatureEngine};
    use serde_json::json;
    use std::sync::Arc;

    fn parse_request(value: serde_json::Value) -> GeminiGenerateContentRequest {
        serde_json::from_value(value).expect("request json must parse")
    }

    fn fallback_patcher() -> SignaturePatcher {
        let engine = Arc::new(ThoughtSignatureEngine::new(3600, 1024));
        SignaturePatcher::new(engine, CacheMissPolicy::Fallback)
    }

    fn fallback_patcher_with_engine() -> (SignaturePatcher, Arc<ThoughtSignatureEngine>) {
        let engine = Arc::new(ThoughtSignatureEngine::new(3600, 1024));
        let p = SignaturePatcher::new(engine.clone(), CacheMissPolicy::Fallback);
        (p, engine)
    }

    #[test]
    fn patch_request_updates_only_model_content_parts() {
        let patcher = fallback_patcher();
        let mut request = parse_request(json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {
                            "thought": true,
                            "text": "ignored user thought"
                        }
                    ]
                },
                {
                    "role": "model",
                    "parts": [
                        {
                            "thought": true,
                            "text": "model thought"
                        }
                    ]
                }
            ]
        }));

        patch_request(&mut request, &patcher);

        assert!(request.contents[0].parts[0].thought_signature.is_none());
        assert_eq!(
            request.contents[1].parts[0].thought_signature.as_deref(),
            Some("skip_thought_signature_validator")
        );
    }

    #[test]
    fn patch_request_uses_cached_signature_for_function_call() {
        let (patcher, engine) = fallback_patcher_with_engine();
        let function_call = json!({
            "name": "get_weather",
            "args": {
                "city": "Berlin",
                "unit": "c"
            }
        });
        let key =
            CacheKeyGenerator::generate_json(&function_call).expect("function call key must exist");
        engine.put_signature(key, Arc::from("sig_fn_001"));

        let mut request = parse_request(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [
                        {
                            "functionCall": {
                                "args": {
                                    "unit": "c",
                                    "city": "Berlin"
                                },
                                "name": "get_weather"
                            }
                        }
                    ]
                }
            ]
        }));

        patch_request(&mut request, &patcher);

        assert_eq!(
            request.contents[0].parts[0].thought_signature.as_deref(),
            Some("sig_fn_001")
        );
    }

    #[test]
    fn patch_request_skips_non_patchable_parts() {
        let patcher = fallback_patcher();
        let mut request = parse_request(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [
                        {
                            "text": "plain model text"
                        }
                    ]
                }
            ]
        }));

        patch_request(&mut request, &patcher);
        assert!(request.contents[0].parts[0].thought_signature.is_none());
    }

    #[test]
    fn patch_request_preserves_client_provided_signature() {
        let patcher = fallback_patcher();
        let mut request = parse_request(json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [
                        {
                            "thought": true,
                            "text": "model thought",
                            "thoughtSignature": "client_sig_123"
                        }
                    ]
                }
            ]
        }));

        patch_request(&mut request, &patcher);

        assert_eq!(
            request.contents[0].parts[0].thought_signature.as_deref(),
            Some("client_sig_123")
        );
    }
}
