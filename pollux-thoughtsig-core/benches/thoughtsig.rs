use std::hint::black_box;
use criterion::{Criterion, criterion_group, criterion_main};
use pollux_thoughtsig_core::{
    CacheKeyGenerator, PatchEvent, SignatureSniffer, SniffEvent, Sniffable, ThoughtSigPatchable,
    ThoughtSignatureEngine,
};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sample_text_short() -> &'static str {
    "The quick brown fox jumps over the lazy dog."
}

fn sample_text_long() -> String {
    "Rust is a systems programming language focused on safety and performance. ".repeat(200)
}

fn sample_json_small() -> Value {
    json!({
        "name": "get_weather",
        "args": { "city": "Berlin", "unit": "c" }
    })
}

fn sample_json_large() -> Value {
    let declarations: Vec<Value> = (0..20)
        .map(|i| {
            json!({
                "name": format!("tool_{i}"),
                "description": format!("A tool that does thing {i} with parameters"),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "arg_a": { "type": "string", "description": "First argument" },
                        "arg_b": { "type": "integer", "description": "Second argument" },
                        "arg_c": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["arg_a"]
                }
            })
        })
        .collect();
    json!(declarations)
}

// ---------------------------------------------------------------------------
// CacheKeyGenerator benchmarks
// ---------------------------------------------------------------------------

fn bench_generate_text_short(c: &mut Criterion) {
    let text = sample_text_short();
    c.bench_function("keygen/text_short", |b| {
        b.iter(|| CacheKeyGenerator::generate_text(black_box(text)))
    });
}

fn bench_generate_text_long(c: &mut Criterion) {
    let text = sample_text_long();
    c.bench_function("keygen/text_long", |b| {
        b.iter(|| CacheKeyGenerator::generate_text(black_box(&text)))
    });
}

fn bench_generate_json_small(c: &mut Criterion) {
    let val = sample_json_small();
    c.bench_function("keygen/json_small", |b| {
        b.iter(|| CacheKeyGenerator::generate_json(black_box(&val)))
    });
}

fn bench_generate_json_large(c: &mut Criterion) {
    let val = sample_json_large();
    c.bench_function("keygen/json_large", |b| {
        b.iter(|| CacheKeyGenerator::generate_json(black_box(&val)))
    });
}

// ---------------------------------------------------------------------------
// ThoughtSignatureEngine benchmarks
// ---------------------------------------------------------------------------

fn bench_engine_put_get(c: &mut Criterion) {
    let engine = ThoughtSignatureEngine::new(3600, 4096);
    let keys: Vec<u64> = (0..1000).collect();
    for &k in &keys {
        engine.put_signature(k, Arc::from(format!("sig_{k}")));
    }

    c.bench_function("engine/cache_hit", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let key = keys[idx % keys.len()];
            idx += 1;
            black_box(engine.get_signature(&key))
        })
    });

    c.bench_function("engine/cache_miss", |b| {
        b.iter(|| black_box(engine.get_signature(&999_999)))
    });

    c.bench_function("engine/put", |b| {
        let mut counter = 100_000u64;
        b.iter(|| {
            counter += 1;
            engine.put_signature(counter, Arc::from("bench_sig"));
        })
    });
}

// ---------------------------------------------------------------------------
// SignatureSniffer benchmarks
// ---------------------------------------------------------------------------

#[allow(dead_code)]
enum DataKind {
    Text(&'static str),
    FunctionCall(Value),
    None,
}

struct FakeSniffable {
    data_kind: DataKind,
    signature: Option<&'static str>,
    index: Option<u32>,
    finished: bool,
}

impl Sniffable for FakeSniffable {
    fn data(&self) -> SniffEvent<'_> {
        match &self.data_kind {
            DataKind::Text(t) => SniffEvent::ThoughtText(t),
            DataKind::FunctionCall(v) => SniffEvent::FunctionCall(v),
            DataKind::None => SniffEvent::None,
        }
    }
    fn thought_signature(&self) -> Option<&str> {
        self.signature
    }
    fn index(&self) -> Option<u32> {
        self.index
    }
    fn is_finished(&self) -> bool {
        self.finished
    }
}

fn bench_sniffer_text_session(c: &mut Criterion) {
    let engine = Arc::new(ThoughtSignatureEngine::new(3600, 4096));

    c.bench_function("sniffer/text_session_3_chunks", |b| {
        b.iter(|| {
            let mut sniffer = SignatureSniffer::new(engine.clone());
            sniffer.inspect(&FakeSniffable {
                data_kind: DataKind::Text("thought chunk alpha "),
                signature: None,
                index: Some(0),
                finished: false,
            });
            sniffer.inspect(&FakeSniffable {
                data_kind: DataKind::Text("thought chunk beta "),
                signature: None,
                index: Some(0),
                finished: false,
            });
            sniffer.inspect(&FakeSniffable {
                data_kind: DataKind::Text("thought chunk gamma"),
                signature: Some("sig_001"),
                index: Some(0),
                finished: true,
            });
        })
    });
}

fn bench_sniffer_function_call(c: &mut Criterion) {
    let engine = Arc::new(ThoughtSignatureEngine::new(3600, 4096));
    let fc = json!({"name": "get_weather", "args": {"city": "Berlin"}});

    c.bench_function("sniffer/function_call", |b| {
        b.iter(|| {
            let mut sniffer = SignatureSniffer::new(engine.clone());
            sniffer.inspect(&FakeSniffable {
                data_kind: DataKind::FunctionCall(fc.clone()),
                signature: Some("sig_fn"),
                index: Some(0),
                finished: true,
            });
        })
    });
}

// ---------------------------------------------------------------------------
// ThoughtSigPatchable benchmarks
// ---------------------------------------------------------------------------

#[allow(dead_code)]
enum FakeData {
    Text(String),
    FunctionCall(Value),
    None,
}

struct FakePatchable {
    data: FakeData,
    signature: Option<String>,
}

impl ThoughtSigPatchable for FakePatchable {
    fn data(&self) -> PatchEvent<'_> {
        match &self.data {
            FakeData::Text(t) => PatchEvent::ThoughtText(t),
            FakeData::FunctionCall(v) => PatchEvent::FunctionCall(v),
            FakeData::None => PatchEvent::None,
        }
    }
    fn thought_signature_mut(&mut self) -> &mut Option<String> {
        &mut self.signature
    }
}

fn bench_patch_cache_hit(c: &mut Criterion) {
    let engine = ThoughtSignatureEngine::new(3600, 4096);
    let text = "alpha beta gamma";
    let key = CacheKeyGenerator::generate_text(text).unwrap();
    engine.put_signature(key, Arc::from("cached_sig"));

    c.bench_function("patch/text_cache_hit", |b| {
        b.iter(|| {
            let mut item = FakePatchable {
                data: FakeData::Text(text.to_string()),
                signature: None,
            };
            black_box(item.patch_thought_signature(&engine));
        })
    });
}

fn bench_patch_cache_miss(c: &mut Criterion) {
    let engine = ThoughtSignatureEngine::new(3600, 4096);

    c.bench_function("patch/text_cache_miss", |b| {
        b.iter(|| {
            let mut item = FakePatchable {
                data: FakeData::Text("never_cached_text".to_string()),
                signature: None,
            };
            black_box(item.patch_thought_signature(&engine));
        })
    });
}

fn bench_patch_json(c: &mut Criterion) {
    let engine = ThoughtSignatureEngine::new(3600, 4096);
    let fc = sample_json_small();

    c.bench_function("patch/json_function_call", |b| {
        b.iter(|| {
            let mut item = FakePatchable {
                data: FakeData::FunctionCall(fc.clone()),
                signature: None,
            };
            black_box(item.patch_thought_signature(&engine));
        })
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    keygen,
    bench_generate_text_short,
    bench_generate_text_long,
    bench_generate_json_small,
    bench_generate_json_large,
);

criterion_group!(engine, bench_engine_put_get,);

criterion_group!(
    sniffer,
    bench_sniffer_text_session,
    bench_sniffer_function_call,
);

criterion_group!(
    patch,
    bench_patch_cache_hit,
    bench_patch_cache_miss,
    bench_patch_json,
);

criterion_main!(keygen, engine, sniffer, patch);
