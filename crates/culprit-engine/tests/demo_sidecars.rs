//! The browser demo loads its sidecars as the TypeScript types generated from the model (`fromRust` in
//! app/src/lib/rust-json.ts). Most are compared with today's output elsewhere (invariants, share, plan_view,
//! plan_samples, dashboard); these three were captured from the demo databases, so each is read back here as the Rust
//! types its TypeScript type is made of. A field the model renames or retypes fails this test, not the browser.

use std::collections::BTreeMap;
use std::path::Path;

use culprit_core::signals::{
    ColumnHistogram, ConstraintIssue, FileIo, IndexContention, QueryCosts, Replica, TableStorage,
};
use culprit_core::waits::Signal;
use culprit_core::{Changes, Finding, IndexStat, QueryStat, Spread, TableStat};
use serde::Deserialize;
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

/// `SpreadSample` in demo-api.ts: per connection, run-time spread by query id and the queries the reports lack.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpreadSample {
    queries: BTreeMap<String, SpreadPatch>,
    extra: Vec<QueryStat>,
}

/// `SpreadPatch`: the spread fields of a `QueryStat`, and of its plan variants by plan id.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpreadPatch {
    spread: Option<Spread>,
    max_time_ms: Option<f64>,
    stddev_time_ms: Option<f64>,
    plans: Option<BTreeMap<String, VariantTimes>>,
}

/// The `PlanVariant` fields a spread patch carries.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantTimes {
    min_time_ms: Option<f64>,
    max_time_ms: Option<f64>,
    stddev_time_ms: Option<f64>,
    compiled: Option<String>,
    variant_query: Option<String>,
}

#[test]
fn captured_run_time_spread_reads_as_query_stats() {
    let spread: BTreeMap<String, SpreadSample> = sidecar("spread.sample.json");
    assert!(spread.values().flat_map(|s| s.queries.values()).any(|q| q.spread.is_some()));
    assert!(spread.values().flat_map(|s| &s.extra).all(|q| !q.id.is_empty()));
    for patch in spread.values().flat_map(|s| s.queries.values()) {
        let SpreadPatch { spread, max_time_ms, stddev_time_ms, plans } = patch;
        assert!(
            spread.is_some() || max_time_ms.is_some() || stddev_time_ms.is_some() || plans.is_some(),
            "an empty patch"
        );
        for v in plans.iter().flat_map(|p| p.values()) {
            let VariantTimes { min_time_ms, max_time_ms, stddev_time_ms, compiled, variant_query } = v;
            if let (Some(min), Some(max)) = (min_time_ms, max_time_ms) {
                assert!(min <= max, "a variant's fastest run is slower than its slowest");
            }
            let said = [stddev_time_ms.is_some(), compiled.is_some(), variant_query.is_some(), min_time_ms.is_some()];
            assert!(said.contains(&true) || max_time_ms.is_some(), "an empty plan patch");
        }
    }
}

/// `SignalsSample` in demo-signals.ts: a note on how the capture was made, and a patch per connection. (serde can't
/// deny unknown fields beside a flattened map; each patch does.)
#[derive(Deserialize)]
struct SignalsSample {
    note: String,
    #[serde(flatten)]
    connections: BTreeMap<String, SignalsPatch>,
}

/// `SignalsPatch`: what the capture adds to a fixture report.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalsPatch {
    costs: BTreeMap<String, QueryCosts>,
    extra: Vec<QueryStat>,
    tables: Vec<TableStat>,
    indexes: Vec<IndexStat>,
    storage: BTreeMap<String, TableStorage>,
    contention: BTreeMap<String, IndexContention>,
    constraints: Vec<ConstraintIssue>,
    histograms: Option<Vec<ColumnHistogram>>,
    files: Vec<FileIo>,
    replicas: Vec<Replica>,
    signals: Vec<Signal>,
    findings: Vec<Finding>,
}

#[test]
fn captured_signals_read_as_the_model() {
    let signals: SignalsSample = sidecar("signals.sample.json");
    assert!(!signals.note.is_empty(), "the capture says how it was made");
    assert!(signals.connections.contains_key("pg-demo"));
    for (name, patch) in &signals.connections {
        let SignalsPatch {
            costs,
            extra,
            tables,
            indexes,
            storage,
            contention,
            constraints,
            histograms,
            files,
            replicas,
            signals,
            findings,
        } = patch;
        let added = costs.len()
            + extra.len()
            + tables.len()
            + indexes.len()
            + storage.len()
            + contention.len()
            + constraints.len()
            + histograms.as_ref().map_or(0, Vec::len)
            + files.len()
            + replicas.len()
            + signals.len()
            + findings.len();
        assert!(added > 0, "{name}: a patch that adds nothing");
    }
}
