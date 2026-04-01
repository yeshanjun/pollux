//! End-to-end benchmarks for the full Pollux request pipeline.
//!
//! These benchmarks exercise the **complete server-side processing chain**:
//!
//!   HTTP request → access_log middleware → RequireKeyAuth middleware
//!   → route dispatch → request body extraction & deserialization
//!   → thought-signature patching → actor credential selection → response
//!
//! The upstream HTTP call is excluded (network-bound, not meaningful locally).
//! Benchmarks that reach the credential selection step end with 503
//! (no credentials loaded), which is the natural boundary.
//!
//! Run with: cargo bench --bench e2e

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::hint::black_box;
use criterion::{Criterion, criterion_group, criterion_main};
use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Fixture: request bodies
// ---------------------------------------------------------------------------

fn gemini_minimal_json() -> String {
    r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#.to_string()
}

fn gemini_full_json() -> String {
    serde_json::json!({
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
            "parts": [{"text": "You are a helpful coding assistant."}]
        },
        "tools": [
            {"functionDeclarations": [
                {"name": "run_command", "description": "Run a shell command", "parameters": {"type": "OBJECT", "properties": {"cmd": {"type": "STRING"}}, "required": ["cmd"]}},
                {"name": "view_file", "description": "View a file", "parameters": {"type": "OBJECT", "properties": {"path": {"type": "STRING"}}, "required": ["path"]}}
            ]}
        ],
        "generationConfig": {
            "temperature": 0.4,
            "maxOutputTokens": 16384,
            "thinkingConfig": {"includeThoughts": true, "thinkingBudget": 1024}
        }
    })
    .to_string()
}

fn codex_json(model: &str) -> String {
    serde_json::json!({
        "model": model,
        "input": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What is Rust?"}
        ],
        "stream": true,
        "store": true,
        "instructions": "Be concise."
    })
    .to_string()
}

fn antigravity_json(model: &str) -> String {
    serde_json::json!({
        "project": "test-project-12345",
        "requestId": "agent/bench/req-001",
        "request": {
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "generationConfig": {
                "temperature": 0.4,
                "maxOutputTokens": 8192,
                "thinkingConfig": {"includeThoughts": true, "thinkingBudget": 1024}
            }
        },
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent"
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// App setup (shared across all benchmarks — single actor registry)
// ---------------------------------------------------------------------------

struct BenchApp {
    router: axum::Router,
    pollux_key: Arc<str>,
    geminicli_model: String,
    codex_model: String,
    antigravity_model: String,
    _db_path: std::path::PathBuf,
}

impl Drop for BenchApp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self._db_path);
    }
}

fn setup_app(rt: &tokio::runtime::Runtime) -> BenchApp {
    rt.block_on(async {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let mut db_path = std::env::temp_dir();
        db_path.push(format!(
            "pollux-bench-e2e-{}-{}.sqlite",
            std::process::id(),
            nanos
        ));
        let database_url = format!("sqlite:{}", db_path.display());

        let db = pollux::db::spawn(&database_url).await;

        let mut cfg = pollux::config::Config::default();
        cfg.basic.pollux_key = "bench-key-e2e".to_string();

        let geminicli_model = pollux::config::CONFIG
            .geminicli()
            .model_list
            .first()
            .cloned()
            .unwrap_or_else(|| "gemini-2.5-pro".to_string());
        cfg.providers.geminicli.model_list = vec![geminicli_model.clone()];

        let codex_model = pollux::config::CONFIG
            .codex()
            .model_list
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-4o-mini".to_string());
        cfg.providers.codex.model_list = vec![codex_model.clone()];

        let antigravity_model = pollux::config::CONFIG
            .antigravity()
            .model_list
            .first()
            .cloned()
            .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
        cfg.providers.antigravity.model_list = vec![antigravity_model.clone()];

        let providers = pollux::providers::Providers::spawn(db, &cfg).await;
        let pollux_key: Arc<str> = Arc::from(cfg.basic.pollux_key.clone());
        let state = pollux::server::router::PolluxState::new(
            providers,
            pollux_key.clone(),
            cfg.basic.insecure_cookie,
        );
        let router = pollux::server::router::pollux_router(state);

        BenchApp {
            router,
            pollux_key,
            geminicli_model,
            codex_model,
            antigravity_model,
            _db_path: db_path,
        }
    })
}

// ---------------------------------------------------------------------------
// All benchmarks in a single function to share one actor registry
// ---------------------------------------------------------------------------

fn bench_e2e(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let app = setup_app(&rt);

    // === 1. Auth middleware ===
    {
        let mut group = c.benchmark_group("e2e/auth");

        group.bench_function("reject_no_key", |b| {
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri("/geminicli/v1beta/models/gemini-2.5-pro:generateContent")
                                .header("content-type", "application/json")
                                .body(Body::from("{}"))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
                    black_box(resp);
                }
            })
        });

        group.bench_function("reject_wrong_key", |b| {
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri("/geminicli/v1beta/models/gemini-2.5-pro:generateContent")
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", "wrong-key")
                                .body(Body::from("{}"))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
                    black_box(resp);
                }
            })
        });

        group.bench_function("accept_valid_key", |b| {
            let key = app.pollux_key.clone();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri("/geminicli/v1beta/models")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::OK);
                    black_box(resp);
                }
            })
        });

        group.finish();
    }

    // === 2. Model list endpoints (auth + routing, no actor call) ===
    {
        let mut group = c.benchmark_group("e2e/models");

        group.bench_function("geminicli_list", |b| {
            let key = app.pollux_key.clone();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri("/geminicli/v1beta/models")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    black_box(resp);
                }
            })
        });

        group.bench_function("codex_list", |b| {
            let key = app.pollux_key.clone();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri("/codex/v1/models")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    black_box(resp);
                }
            })
        });

        group.bench_function("geminicli_openai_list", |b| {
            let key = app.pollux_key.clone();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri("/geminicli/v1beta/openai/models")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    black_box(resp);
                }
            })
        });

        group.finish();
    }

    // === 3. Full pipeline: auth → extract → actor → 503 (no credentials) ===
    {
        let mut group = c.benchmark_group("e2e/pipeline");

        // GeminiCli: minimal body
        group.bench_function("geminicli_minimal", |b| {
            let key = app.pollux_key.clone();
            let uri = format!(
                "/geminicli/v1beta/models/{}:generateContent",
                app.geminicli_model
            );
            let body = gemini_minimal_json();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let uri = uri.clone();
                let body = body.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(&uri)
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(body))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                    black_box(resp);
                }
            })
        });

        // GeminiCli: full body (system instructions, tools, generation config)
        group.bench_function("geminicli_full", |b| {
            let key = app.pollux_key.clone();
            let uri = format!(
                "/geminicli/v1beta/models/{}:generateContent",
                app.geminicli_model
            );
            let body = gemini_full_json();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let uri = uri.clone();
                let body = body.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(&uri)
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(body))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                    black_box(resp);
                }
            })
        });

        // GeminiCli: stream endpoint
        group.bench_function("geminicli_stream", |b| {
            let key = app.pollux_key.clone();
            let uri = format!(
                "/geminicli/v1beta/models/{}:streamGenerateContent",
                app.geminicli_model
            );
            let body = gemini_minimal_json();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let uri = uri.clone();
                let body = body.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(&uri)
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(body))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                    black_box(resp);
                }
            })
        });

        // Codex
        group.bench_function("codex", |b| {
            let key = app.pollux_key.clone();
            let body = codex_json(&app.codex_model);
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let body = body.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri("/codex/v1/responses")
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(body))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                    black_box(resp);
                }
            })
        });

        // Antigravity
        group.bench_function("antigravity", |b| {
            let key = app.pollux_key.clone();
            let uri = format!(
                "/antigravity/v1beta/models/{}:generateContent",
                app.antigravity_model
            );
            let body = antigravity_json(&app.antigravity_model);
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let uri = uri.clone();
                let body = body.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(&uri)
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(body))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                    black_box(resp);
                }
            })
        });

        group.finish();
    }

    // === 4. Error paths ===
    {
        let mut group = c.benchmark_group("e2e/errors");

        // Unsupported model → 400
        group.bench_function("unsupported_model", |b| {
            let key = app.pollux_key.clone();
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(
                                    "/geminicli/v1beta/models/nonexistent-model-xyz:generateContent",
                                )
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from(gemini_minimal_json()))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
                    black_box(resp);
                }
            })
        });

        // Invalid JSON body → 400
        group.bench_function("invalid_json", |b| {
            let key = app.pollux_key.clone();
            let uri = format!(
                "/geminicli/v1beta/models/{}:generateContent",
                app.geminicli_model
            );
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                let key = key.clone();
                let uri = uri.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(&uri)
                                .header("content-type", "application/json")
                                .header("x-goog-api-key", key.as_ref())
                                .body(Body::from("not-json"))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
                    black_box(resp);
                }
            })
        });

        // 404 fallback
        group.bench_function("not_found", |b| {
            b.to_async(&rt).iter(|| {
                let router = app.router.clone();
                async move {
                    let resp = router
                        .oneshot(
                            Request::builder()
                                .method("GET")
                                .uri("/nonexistent/path")
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    debug_assert_eq!(resp.status(), StatusCode::NOT_FOUND);
                    black_box(resp);
                }
            })
        });

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_e2e);
criterion_main!(benches);
