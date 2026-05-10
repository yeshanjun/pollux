#![allow(
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::used_underscore_binding,
    clippy::duration_suboptimal_units,
    clippy::redundant_closure_for_method_calls
)]
use criterion::{Criterion, criterion_group, criterion_main};
use pollux::model_catalog::{ModelCapabilities, ModelRegistry};
use std::hint::black_box;

// ---------------------------------------------------------------------------
// Fixture data
// ---------------------------------------------------------------------------

fn sample_model_names(count: usize) -> Vec<String> {
    let base_names = [
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.0-flash",
        "gemini-2.0-flash-lite",
        "claude-sonnet-4-5-thinking",
        "claude-sonnet-4-5",
        "claude-haiku-3-5",
        "o3-mini",
        "o4-mini",
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4.1",
        "gpt-4.1-mini",
        "gpt-4.1-nano",
        "codex-mini",
        "gemini-2.5-pro-preview",
    ];
    base_names
        .iter()
        .take(count)
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// ModelRegistry benchmarks
// ---------------------------------------------------------------------------

fn bench_registry_new(c: &mut Criterion) {
    let names = sample_model_names(16);
    c.bench_function("registry/new_16_models", |b| {
        b.iter(|| ModelRegistry::new(black_box(&names)))
    });
}

fn bench_registry_lookup_hit(c: &mut Criterion) {
    let names = sample_model_names(16);
    let registry = ModelRegistry::new(&names);
    c.bench_function("registry/get_index_hit", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let name = &names[idx % names.len()];
            idx += 1;
            black_box(registry.get_index(name))
        })
    });
}

fn bench_registry_lookup_miss(c: &mut Criterion) {
    let names = sample_model_names(16);
    let registry = ModelRegistry::new(&names);
    c.bench_function("registry/get_index_miss", |b| {
        b.iter(|| black_box(registry.get_index("nonexistent-model-xyz")))
    });
}

fn bench_registry_reverse_lookup(c: &mut Criterion) {
    let names = sample_model_names(16);
    let registry = ModelRegistry::new(&names);
    c.bench_function("registry/get_name", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let i = idx % registry.len();
            idx += 1;
            black_box(registry.get_name(i))
        })
    });
}

// ---------------------------------------------------------------------------
// ModelCapabilities benchmarks
// ---------------------------------------------------------------------------

fn bench_capabilities_supports(c: &mut Criterion) {
    let mut caps = ModelCapabilities::none();
    for i in (0..16).step_by(2) {
        caps.enable(i);
    }
    c.bench_function("capabilities/supports", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let i = idx % 16;
            idx += 1;
            black_box(caps.supports(i))
        })
    });
}

fn bench_capabilities_contains_all(c: &mut Criterion) {
    let caps = ModelCapabilities::from_bits(0b1111_1111_1111_1111);
    let required = ModelCapabilities::from_bits(0b0000_0101_0101_0101);
    c.bench_function("capabilities/contains_all", |b| {
        b.iter(|| black_box(caps.contains_all(black_box(required))))
    });
}

fn bench_capabilities_intersects(c: &mut Criterion) {
    let a = ModelCapabilities::from_bits(0b1010_1010);
    let b_caps = ModelCapabilities::from_bits(0b0101_0101);
    let c_caps = ModelCapabilities::from_bits(0b1000_0000);
    c.bench_function("capabilities/intersects_disjoint", |b| {
        b.iter(|| black_box(a.intersects(black_box(b_caps))))
    });
    c.bench_function("capabilities/intersects_overlap", |b| {
        b.iter(|| black_box(a.intersects(black_box(c_caps))))
    });
}

fn bench_capabilities_merge(c: &mut Criterion) {
    let a = ModelCapabilities::from_bits(0b1010_1010);
    let b = ModelCapabilities::from_bits(0b0101_0101);
    c.bench_function("capabilities/merge", |b_iter| {
        b_iter.iter(|| black_box(a.merge(black_box(b))))
    });
}

fn bench_capabilities_disable_mask(c: &mut Criterion) {
    c.bench_function("capabilities/disable_mask", |b| {
        b.iter(|| {
            let mut caps = ModelCapabilities::all();
            caps.disable_mask(black_box(0b1111_0000));
            black_box(caps)
        })
    });
}

// ---------------------------------------------------------------------------
// Combined mask-to-name resolution (simulates model_names_from_mask)
// ---------------------------------------------------------------------------

fn bench_mask_to_names(c: &mut Criterion) {
    let names = sample_model_names(16);
    let registry = ModelRegistry::new(&names);
    let model_mask = 0b1010_0101_0011_1100u64;

    c.bench_function("registry/mask_to_names", |b| {
        b.iter(|| {
            let mut result = Vec::new();
            for idx in 0..registry.len() {
                let bit = 1u64 << idx;
                if (black_box(model_mask) & bit) != 0 {
                    result.push(registry.get_name(idx).to_string());
                }
            }
            black_box(result)
        })
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    registry,
    bench_registry_new,
    bench_registry_lookup_hit,
    bench_registry_lookup_miss,
    bench_registry_reverse_lookup,
    bench_mask_to_names,
);

criterion_group!(
    capabilities,
    bench_capabilities_supports,
    bench_capabilities_contains_all,
    bench_capabilities_intersects,
    bench_capabilities_merge,
    bench_capabilities_disable_mask,
);

criterion_main!(registry, capabilities);
