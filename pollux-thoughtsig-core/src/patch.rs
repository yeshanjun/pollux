use crate::{CacheKey, CacheKeyGenerator, ThoughtSignatureEngine};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;

/// Zero-allocation truncated view of a signature for logging.
pub struct SignaturePreview<'a>(pub &'a str);

const PREVIEW_MAX: usize = 48;

impl fmt::Display for SignaturePreview<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.len() <= PREVIEW_MAX {
            f.write_str(self.0)
        } else {
            f.write_str(&self.0[..PREVIEW_MAX])?;
            f.write_str("...")
        }
    }
}

pub enum PatchEvent<'a> {
    ThoughtText(&'a str),
    FunctionCall(&'a Value),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMissPolicy {
    /// Use the fallback (dummy) signature so the part is kept in the request.
    Fallback,
    /// Signal that the part should be dropped from the request.
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOutcome {
    Skipped,
    Patched { cache_key: Option<CacheKey> },
    Dropped { cache_key: Option<CacheKey> },
}

/// Pure data-access trait for types that carry a thought signature slot.
///
/// Implementors only describe how to read/write the underlying data.
/// All patch logic lives in [`SignaturePatcher`].
pub trait Patchable {
    fn data(&self) -> PatchEvent<'_>;
    fn thought_signature(&self) -> Option<&str>;
    fn thought_signature_mut(&mut self) -> &mut Option<String>;
}

/// Orchestrator that decides how to fill thought signatures on [`Patchable`] items.
///
/// Mirrors [`crate::SignatureSniffer`]: the trait describes data shape,
/// the struct owns the engine reference and policy.
pub struct SignaturePatcher {
    engine: Arc<ThoughtSignatureEngine>,
    policy: CacheMissPolicy,
}

impl SignaturePatcher {
    pub fn new(engine: Arc<ThoughtSignatureEngine>, policy: CacheMissPolicy) -> Self {
        Self { engine, policy }
    }

    pub fn patch<T: Patchable>(&self, item: &mut T) -> PatchOutcome {
        // Client already provided a signature — pass through untouched.
        if item.thought_signature().is_some() {
            return PatchOutcome::Skipped;
        }

        let cache_key = match item.data() {
            PatchEvent::ThoughtText(text) => CacheKeyGenerator::generate_text(text),
            PatchEvent::FunctionCall(function_call) => {
                CacheKeyGenerator::generate_json(function_call)
            }
            PatchEvent::None => return PatchOutcome::Skipped,
        };

        // Cache hit — use the cached signature.
        if let Some(signature) = cache_key.and_then(|k| self.engine.get_signature(&k)) {
            *item.thought_signature_mut() = Some(signature.to_string());
            return PatchOutcome::Patched { cache_key };
        }

        // Cache miss — delegate to policy.
        match self.policy {
            CacheMissPolicy::Fallback => {
                *item.thought_signature_mut() = Some(self.engine.fallback_signature().to_string());
                PatchOutcome::Patched { cache_key }
            }
            CacheMissPolicy::Drop => PatchOutcome::Dropped { cache_key },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    enum FakeData {
        Text(&'static str),
        FunctionCall(Value),
        None,
    }

    struct FakePatchable {
        data: FakeData,
        signature: Option<String>,
    }

    impl FakePatchable {
        fn new(data: FakeData) -> Self {
            Self {
                data,
                signature: None,
            }
        }

        fn with_signature(mut self, sig: &str) -> Self {
            self.signature = Some(sig.to_string());
            self
        }
    }

    impl Patchable for FakePatchable {
        fn data(&self) -> PatchEvent<'_> {
            match &self.data {
                FakeData::Text(text) => PatchEvent::ThoughtText(text),
                FakeData::FunctionCall(function_call) => PatchEvent::FunctionCall(function_call),
                FakeData::None => PatchEvent::None,
            }
        }

        fn thought_signature(&self) -> Option<&str> {
            self.signature.as_deref()
        }

        fn thought_signature_mut(&mut self) -> &mut Option<String> {
            &mut self.signature
        }
    }

    fn patcher(policy: CacheMissPolicy) -> SignaturePatcher {
        let engine = Arc::new(ThoughtSignatureEngine::new(3600, 1024));
        SignaturePatcher::new(engine, policy)
    }

    fn patcher_with_engine(
        policy: CacheMissPolicy,
    ) -> (SignaturePatcher, Arc<ThoughtSignatureEngine>) {
        let engine = Arc::new(ThoughtSignatureEngine::new(3600, 1024));
        let p = SignaturePatcher::new(engine.clone(), policy);
        (p, engine)
    }

    #[test]
    fn patch_text_with_cache_hit_uses_cached_signature() {
        let (patcher, engine) = patcher_with_engine(CacheMissPolicy::Fallback);
        let key = CacheKeyGenerator::generate_text("alpha").expect("text key must exist");
        engine.put_signature(key, Arc::from("sig_alpha"));

        let mut item = FakePatchable::new(FakeData::Text("alpha"));
        let outcome = patcher.patch(&mut item);
        assert_eq!(
            outcome,
            PatchOutcome::Patched {
                cache_key: Some(key)
            }
        );
        assert_eq!(item.signature.as_deref(), Some("sig_alpha"));
    }

    #[test]
    fn fallback_policy_uses_dummy_on_cache_miss() {
        let patcher = patcher(CacheMissPolicy::Fallback);
        let function_call = json!({
            "name": "get_weather",
            "args": { "city": "Berlin" }
        });

        let mut item = FakePatchable::new(FakeData::FunctionCall(function_call.clone()));
        let outcome = patcher.patch(&mut item);
        assert_eq!(
            outcome,
            PatchOutcome::Patched {
                cache_key: CacheKeyGenerator::generate_json(&function_call),
            }
        );
        assert_eq!(
            item.signature.as_deref(),
            Some("skip_thought_signature_validator")
        );
    }

    #[test]
    fn drop_policy_drops_on_cache_miss() {
        let patcher = patcher(CacheMissPolicy::Drop);
        let mut item = FakePatchable::new(FakeData::Text("uncached"));
        let outcome = patcher.patch(&mut item);
        assert_eq!(
            outcome,
            PatchOutcome::Dropped {
                cache_key: CacheKeyGenerator::generate_text("uncached"),
            }
        );
        assert!(item.signature.is_none());
    }

    #[test]
    fn existing_signature_is_passed_through() {
        let patcher = patcher(CacheMissPolicy::Fallback);
        let mut item =
            FakePatchable::new(FakeData::Text("whatever")).with_signature("client_provided");
        let outcome = patcher.patch(&mut item);
        assert_eq!(outcome, PatchOutcome::Skipped);
        assert_eq!(item.signature.as_deref(), Some("client_provided"));
    }

    #[test]
    fn patch_none_event_is_skipped() {
        let patcher = patcher(CacheMissPolicy::Fallback);
        let mut item = FakePatchable::new(FakeData::None).with_signature("keep_me");
        let outcome = patcher.patch(&mut item);
        assert_eq!(outcome, PatchOutcome::Skipped);
        assert_eq!(item.signature.as_deref(), Some("keep_me"));
    }

    #[test]
    fn fallback_policy_uses_dummy_for_empty_text() {
        let patcher = patcher(CacheMissPolicy::Fallback);
        let mut item = FakePatchable::new(FakeData::Text("   "));
        let outcome = patcher.patch(&mut item);
        assert_eq!(outcome, PatchOutcome::Patched { cache_key: None });
        assert_eq!(
            item.signature.as_deref(),
            Some("skip_thought_signature_validator")
        );
    }

    #[test]
    fn drop_policy_drops_empty_text() {
        let patcher = patcher(CacheMissPolicy::Drop);
        let mut item = FakePatchable::new(FakeData::Text("   "));
        let outcome = patcher.patch(&mut item);
        assert_eq!(outcome, PatchOutcome::Dropped { cache_key: None });
        assert!(item.signature.is_none());
    }
}
