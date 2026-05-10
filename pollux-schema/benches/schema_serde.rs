#![allow(
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::used_underscore_binding,
    clippy::duration_suboptimal_units,
    clippy::redundant_closure_for_method_calls
)]
use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::hint::black_box;

use pollux_schema::antigravity::AntigravityRequestBody;
use pollux_schema::codex::CodexRequestBody;
use pollux_schema::gemini::GeminiGenerateContentRequest;
use pollux_schema::openai::OpenaiRequestBody;

// ---------------------------------------------------------------------------
// Fixture data
// ---------------------------------------------------------------------------

fn gemini_minimal() -> Value {
    json!({
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
    })
}

fn gemini_full() -> Value {
    json!({
        "contents": [
            {"role": "user", "parts": [{"text": "user info block"}]},
            {"role": "model", "parts": [
                {"thought": true, "text": "let me think about this..."},
                {"text": "Here is my response."}
            ]},
            {"role": "user", "parts": [{"text": "step 0: user request"}]},
        ],
        "systemInstruction": {
            "role": "user",
            "parts": [{"text": "You are a helpful coding assistant. Follow the user's instructions carefully."}]
        },
        "tools": [
            {"functionDeclarations": [
                {"name": "run_command", "description": "Run a shell command", "parameters": {"type": "OBJECT", "properties": {"cmd": {"type": "STRING"}}, "required": ["cmd"]}},
                {"name": "view_file", "description": "View a file", "parameters": {"type": "OBJECT", "properties": {"path": {"type": "STRING"}}, "required": ["path"]}},
                {"name": "edit_file", "description": "Edit a file", "parameters": {"type": "OBJECT", "properties": {"path": {"type": "STRING"}, "content": {"type": "STRING"}}, "required": ["path", "content"]}}
            ]},
            {"functionDeclarations": [
                {"name": "search_code", "description": "Search codebase", "parameters": {"type": "OBJECT", "properties": {"query": {"type": "STRING"}}, "required": ["query"]}}
            ]}
        ],
        "toolConfig": {
            "functionCallingConfig": {"mode": "VALIDATED"}
        },
        "generationConfig": {
            "temperature": 0.4,
            "topP": 1.0,
            "topK": 50,
            "candidateCount": 1,
            "maxOutputTokens": 16384,
            "stopSequences": ["<|user|>", "<|bot|>", "<|endoftext|>"],
            "thinkingConfig": {
                "includeThoughts": true,
                "thinkingBudget": 1024
            }
        },
        "sessionId": "-3750763034362895579"
    })
}

fn gemini_multi_turn_20() -> Value {
    let mut contents = Vec::new();
    for i in 0..10 {
        contents.push(json!({
            "role": "user",
            "parts": [{"text": format!("User message {i}: Can you help me debug this function? The error says 'lifetime mismatch' and I'm not sure what's going on.")}]
        }));
        contents.push(json!({
            "role": "model",
            "parts": [
                {"thought": true, "text": format!("Thinking about turn {i}... The user is asking about lifetime issues in Rust. Let me analyze the code they're referring to.")},
                {"text": format!("Model response {i}: The lifetime error occurs because the borrow checker cannot verify that the reference outlives the scope. Try adding an explicit lifetime parameter.")}
            ]
        }));
    }
    json!({
        "contents": contents,
        "generationConfig": {
            "temperature": 0.7,
            "maxOutputTokens": 8192,
            "thinkingConfig": {"thinkingBudget": 2048}
        }
    })
}

fn antigravity_envelope() -> Value {
    json!({
        "project": "test-project-12345",
        "requestId": "agent/1770489747018/b9acb5be-0d95-407e-a9cf-94315ff8a43e",
        "request": gemini_full(),
        "model": "claude-sonnet-4-5-thinking",
        "userAgent": "antigravity",
        "requestType": "agent"
    })
}

fn openai_simple() -> Value {
    json!({
        "model": "gpt-4o-mini",
        "input": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What is Rust?"}
        ],
        "stream": true,
        "store": true,
        "instructions": "Be concise."
    })
}

fn openai_complex() -> Value {
    let mut input = vec![
        json!({"role": "system", "content": "System preamble with coding guidelines."}),
        json!({"role": "system", "content": [
            {"type": "input_text", "text": "Additional system context block 1."},
            {"type": "input_text", "text": "Additional system context block 2."}
        ]}),
    ];
    for i in 0..8 {
        input.push(json!({
            "role": "user",
            "content": format!("User turn {i}: Please explain the borrow checker in Rust.")
        }));
        input.push(json!({
            "role": "assistant",
            "content": null,
            "encrypted_content": format!("gAAAA-encrypted-reasoning-{i}")
        }));
    }
    json!({
        "model": "o3-mini",
        "input": input,
        "instructions": "Explicit instructions for the model.",
        "reasoning": {"effort": "high"},
        "include": ["reasoning.encrypted_content"],
        "stream": true,
        "temperature": 0.8,
        "max_output_tokens": 4096
    })
}

// ---------------------------------------------------------------------------
// Gemini schema benchmarks
// ---------------------------------------------------------------------------

fn bench_gemini_deser_minimal(c: &mut Criterion) {
    let raw = serde_json::to_string(&gemini_minimal()).unwrap();
    c.bench_function("gemini/deser_minimal", |b| {
        b.iter(|| serde_json::from_str::<GeminiGenerateContentRequest>(black_box(&raw)).unwrap())
    });
}

fn bench_gemini_deser_full(c: &mut Criterion) {
    let raw = serde_json::to_string(&gemini_full()).unwrap();
    c.bench_function("gemini/deser_full", |b| {
        b.iter(|| serde_json::from_str::<GeminiGenerateContentRequest>(black_box(&raw)).unwrap())
    });
}

fn bench_gemini_deser_multi_turn(c: &mut Criterion) {
    let raw = serde_json::to_string(&gemini_multi_turn_20()).unwrap();
    c.bench_function("gemini/deser_multi_turn_20", |b| {
        b.iter(|| serde_json::from_str::<GeminiGenerateContentRequest>(black_box(&raw)).unwrap())
    });
}

fn bench_gemini_ser_full(c: &mut Criterion) {
    let raw = serde_json::to_string(&gemini_full()).unwrap();
    let req: GeminiGenerateContentRequest = serde_json::from_str(&raw).unwrap();
    c.bench_function("gemini/ser_full", |b| {
        b.iter(|| serde_json::to_string(black_box(&req)).unwrap())
    });
}

fn bench_gemini_roundtrip_full(c: &mut Criterion) {
    let raw = serde_json::to_string(&gemini_full()).unwrap();
    c.bench_function("gemini/roundtrip_full", |b| {
        b.iter(|| {
            let req: GeminiGenerateContentRequest = serde_json::from_str(black_box(&raw)).unwrap();
            serde_json::to_string(&req).unwrap()
        })
    });
}

// ---------------------------------------------------------------------------
// Antigravity schema benchmarks
// ---------------------------------------------------------------------------

fn bench_antigravity_deser(c: &mut Criterion) {
    let raw = serde_json::to_string(&antigravity_envelope()).unwrap();
    c.bench_function("antigravity/deser_envelope", |b| {
        b.iter(|| serde_json::from_str::<AntigravityRequestBody>(black_box(&raw)).unwrap())
    });
}

fn bench_antigravity_ser(c: &mut Criterion) {
    let raw = serde_json::to_string(&antigravity_envelope()).unwrap();
    let body: AntigravityRequestBody = serde_json::from_str(&raw).unwrap();
    c.bench_function("antigravity/ser_envelope", |b| {
        b.iter(|| serde_json::to_string(black_box(&body)).unwrap())
    });
}

// ---------------------------------------------------------------------------
// OpenAI / Codex schema benchmarks
// ---------------------------------------------------------------------------

fn bench_openai_deser_simple(c: &mut Criterion) {
    let raw = serde_json::to_string(&openai_simple()).unwrap();
    c.bench_function("openai/deser_simple", |b| {
        b.iter(|| serde_json::from_str::<OpenaiRequestBody>(black_box(&raw)).unwrap())
    });
}

fn bench_openai_deser_complex(c: &mut Criterion) {
    let raw = serde_json::to_string(&openai_complex()).unwrap();
    c.bench_function("openai/deser_complex", |b| {
        b.iter(|| serde_json::from_str::<OpenaiRequestBody>(black_box(&raw)).unwrap())
    });
}

fn bench_codex_transform_simple(c: &mut Criterion) {
    let raw = serde_json::to_string(&openai_simple()).unwrap();
    c.bench_function("codex/transform_simple", |b| {
        b.iter(|| {
            let body: OpenaiRequestBody = serde_json::from_str(&raw).unwrap();
            let _codex: CodexRequestBody = black_box(body).into();
        })
    });
}

fn bench_codex_transform_complex(c: &mut Criterion) {
    let raw = serde_json::to_string(&openai_complex()).unwrap();
    c.bench_function("codex/transform_complex", |b| {
        b.iter(|| {
            let body: OpenaiRequestBody = serde_json::from_str(&raw).unwrap();
            let _codex: CodexRequestBody = black_box(body).into();
        })
    });
}

fn bench_codex_full_pipeline(c: &mut Criterion) {
    let raw = serde_json::to_string(&openai_complex()).unwrap();
    c.bench_function("codex/full_pipeline_deser_transform_ser", |b| {
        b.iter(|| {
            let body: OpenaiRequestBody = serde_json::from_str(black_box(&raw)).unwrap();
            let codex: CodexRequestBody = body.into();
            serde_json::to_string(&codex).unwrap()
        })
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    gemini,
    bench_gemini_deser_minimal,
    bench_gemini_deser_full,
    bench_gemini_deser_multi_turn,
    bench_gemini_ser_full,
    bench_gemini_roundtrip_full,
);

criterion_group!(antigravity, bench_antigravity_deser, bench_antigravity_ser,);

criterion_group!(
    openai_codex,
    bench_openai_deser_simple,
    bench_openai_deser_complex,
    bench_codex_transform_simple,
    bench_codex_transform_complex,
    bench_codex_full_pipeline,
);

criterion_main!(gemini, antigravity, openai_codex);
