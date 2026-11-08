//! The browser demo loads its sidecars as the TypeScript types generated from the model (`fromRust` in
//! app/src/lib/rust-json.ts). Most are compared with today's output elsewhere (invariants, share, plan_view,
//! plan_samples, dashboard); this one was captured from the demo databases, so it is read back here as the Rust types
//! its TypeScript type is made of. A field the model renames or retypes fails this test, not the browser.

use std::collections::BTreeMap;
use std::path::Path;

use culprit_core::Changes;
use serde::de::DeserializeOwned;

fn sidecar<T: DeserializeOwned>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name} no longer reads as the model's types: {e}"))
}

#[test]
fn captured_period_comparisons_read_as_changes() {
    let changes: BTreeMap<String, Changes> = sidecar("changes.sample.json");
    assert!(changes.contains_key("pg-demo"), "{:?}", changes.keys().collect::<Vec<_>>());
}
