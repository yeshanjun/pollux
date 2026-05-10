#![allow(
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::used_underscore_binding,
    clippy::duration_suboptimal_units,
    clippy::redundant_closure_for_method_calls
)]
//! Benchmarks for the credential scheduler hot path.
//!
//! Run with: cargo bench --features bench --bench scheduler

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;

use pollux::model_catalog::ModelCapabilities;
use pollux::providers::geminicli::resource::GeminiCliResource;
use pollux::providers::traits::scheduler::ResourceScheduler;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mask(index: usize) -> u64 {
    1u64 << index
}

fn make_credential(project_id: &str) -> GeminiCliResource {
    GeminiCliResource::from_payload(json!({
        "email": null,
        "project_id": project_id,
        "refresh_token": "refresh_token_placeholder",
        "access_token": "access_token_placeholder",
        "expiry": Utc::now() + chrono::Duration::minutes(30),
    }))
    .expect("valid resource payload")
}

fn make_expired_credential(project_id: &str) -> GeminiCliResource {
    GeminiCliResource::from_payload(json!({
        "email": null,
        "project_id": project_id,
        "refresh_token": "refresh_token_placeholder",
        "access_token": "access_token_placeholder",
        "expiry": Utc::now() - chrono::Duration::minutes(10),
    }))
    .expect("valid resource payload")
}

fn setup_manager(model_count: usize, cred_count: u64) -> ResourceScheduler<GeminiCliResource> {
    let mut manager = ResourceScheduler::<GeminiCliResource>::new(model_count);
    let all_caps = ModelCapabilities::all().bits();
    for id in 1..=cred_count {
        manager.add_credential(id, make_credential(&format!("proj-{id}")), all_caps);
    }
    manager
}

// ---------------------------------------------------------------------------
// get_assigned benchmarks (the single hottest path)
// ---------------------------------------------------------------------------

fn bench_get_assigned_single_cred(c: &mut Criterion) {
    let mut manager = setup_manager(4, 1);

    c.bench_function("scheduler/get_assigned_1_cred", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_10_creds(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);

    c.bench_function("scheduler/get_assigned_10_creds", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_100_creds(c: &mut Criterion) {
    let mut manager = setup_manager(8, 100);

    c.bench_function("scheduler/get_assigned_100_creds", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_round_robin(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);

    c.bench_function("scheduler/round_robin_10_creds", |b| {
        b.iter(|| {
            // Simulate 10 sequential assignments (full rotation)
            for _ in 0..10 {
                black_box(manager.get_assigned(mask(0), None));
            }
        })
    });
}

fn bench_get_assigned_different_models(c: &mut Criterion) {
    let mut manager = setup_manager(8, 20);

    c.bench_function("scheduler/get_assigned_rotating_models", |b| {
        let mut model_idx = 0usize;
        b.iter(|| {
            let m = mask(model_idx % 8);
            model_idx += 1;
            black_box(manager.get_assigned(m, None))
        })
    });
}

// ---------------------------------------------------------------------------
// get_assigned with mixed credential states
// ---------------------------------------------------------------------------

fn bench_get_assigned_with_expired(c: &mut Criterion) {
    let mut manager = ResourceScheduler::<GeminiCliResource>::new(4);
    let all_caps = ModelCapabilities::all().bits();

    // Add 5 expired + 5 valid credentials
    for id in 1..=5 {
        manager.add_credential(
            id,
            make_expired_credential(&format!("proj-expired-{id}")),
            all_caps,
        );
    }
    for id in 6..=10 {
        manager.add_credential(id, make_credential(&format!("proj-valid-{id}")), all_caps);
    }

    c.bench_function("scheduler/get_assigned_skip_expired", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_with_refreshing(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);
    // Mark half as refreshing
    for id in 1..=5 {
        manager.mark_refreshing(id);
    }

    c.bench_function("scheduler/get_assigned_skip_refreshing", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_with_unsupported(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);
    // Mark half as unsupported for model 0
    for id in 1..=5 {
        manager.mark_model_unsupported(id, mask(0));
    }

    c.bench_function("scheduler/get_assigned_skip_unsupported", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

// ---------------------------------------------------------------------------
// Cooldown / waiting room benchmarks
// ---------------------------------------------------------------------------

fn bench_cooldown_report_and_drain(c: &mut Criterion) {
    c.bench_function("scheduler/report_rate_limit", |b| {
        let mut manager = setup_manager(4, 10);
        let mut counter = 0u64;
        b.iter(|| {
            let id = (counter % 10) + 1;
            counter += 1;
            manager.report_rate_limit(id, mask(0), Duration::from_secs(60));
        })
    });
}

fn bench_process_waiting_room_empty(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);

    c.bench_function("scheduler/get_assigned_empty_waitroom", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_process_waiting_room_with_expired_cooldowns(c: &mut Criterion) {
    c.bench_function("scheduler/drain_expired_cooldowns", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut manager = setup_manager(4, 20);
                // Add cooldowns in the past (will be drained immediately)
                for id in 1..=20 {
                    manager.report_rate_limit(id, mask(0), Duration::from_nanos(1));
                }
                // Give the cooldowns time to expire
                std::thread::sleep(Duration::from_micros(10));

                let start = std::time::Instant::now();
                black_box(manager.get_assigned(mask(0), None));
                total += start.elapsed();
            }
            total
        })
    });
}

// ---------------------------------------------------------------------------
// Credential management benchmarks
// ---------------------------------------------------------------------------

fn bench_add_credential(c: &mut Criterion) {
    c.bench_function("scheduler/add_credential_4_models", |b| {
        let mut manager = ResourceScheduler::<GeminiCliResource>::new(4);
        let all_caps = ModelCapabilities::all().bits();
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            manager.add_credential(
                counter,
                make_credential(&format!("proj-{counter}")),
                all_caps,
            );
        })
    });
}

fn bench_add_credential_16_models(c: &mut Criterion) {
    c.bench_function("scheduler/add_credential_16_models", |b| {
        let mut manager = ResourceScheduler::<GeminiCliResource>::new(16);
        let all_caps = ModelCapabilities::all().bits();
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            manager.add_credential(
                counter,
                make_credential(&format!("proj-{counter}")),
                all_caps,
            );
        })
    });
}

fn bench_delete_credential(c: &mut Criterion) {
    c.bench_function("scheduler/delete_credential", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut manager = setup_manager(4, 20);
                let start = std::time::Instant::now();
                manager.delete_credential(10);
                total += start.elapsed();
            }
            total
        })
    });
}

// ---------------------------------------------------------------------------
// Contention simulation: empty queue
// ---------------------------------------------------------------------------

fn bench_get_assigned_1000_creds(c: &mut Criterion) {
    let mut manager = setup_manager(8, 1000);

    c.bench_function("scheduler/get_assigned_1000_creds", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_round_robin_1000(c: &mut Criterion) {
    let mut manager = setup_manager(8, 1000);

    c.bench_function("scheduler/round_robin_1000_creds", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                black_box(manager.get_assigned(mask(0), None));
            }
        })
    });
}

fn bench_get_assigned_all_exhausted(c: &mut Criterion) {
    let mut manager = ResourceScheduler::<GeminiCliResource>::new(4);
    // No credentials at all
    c.bench_function("scheduler/get_assigned_empty", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_all_cooling(c: &mut Criterion) {
    let mut manager = setup_manager(4, 10);
    for id in 1..=10 {
        manager.report_rate_limit(id, mask(0), Duration::from_secs(3600));
    }
    c.bench_function("scheduler/get_assigned_all_cooling_10", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

fn bench_get_assigned_all_cooling_1000(c: &mut Criterion) {
    let mut manager = setup_manager(8, 1000);
    for id in 1..=1000 {
        manager.report_rate_limit(id, mask(0), Duration::from_secs(3600));
    }
    c.bench_function("scheduler/get_assigned_all_cooling_1000", |b| {
        b.iter(|| black_box(manager.get_assigned(mask(0), None)))
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    assignment,
    bench_get_assigned_single_cred,
    bench_get_assigned_10_creds,
    bench_get_assigned_100_creds,
    bench_get_assigned_round_robin,
    bench_get_assigned_different_models,
);

criterion_group!(
    mixed_states,
    bench_get_assigned_with_expired,
    bench_get_assigned_with_refreshing,
    bench_get_assigned_with_unsupported,
);

criterion_group!(
    cooldown,
    bench_cooldown_report_and_drain,
    bench_process_waiting_room_empty,
    bench_process_waiting_room_with_expired_cooldowns,
);

criterion_group!(
    management,
    bench_add_credential,
    bench_add_credential_16_models,
    bench_delete_credential,
);

criterion_group!(
    scale,
    bench_get_assigned_1000_creds,
    bench_get_assigned_round_robin_1000,
);

criterion_group!(
    exhaustion,
    bench_get_assigned_all_exhausted,
    bench_get_assigned_all_cooling,
    bench_get_assigned_all_cooling_1000,
);

criterion_main!(
    assignment,
    mixed_states,
    cooldown,
    management,
    scale,
    exhaustion
);
