use super::*;
use culprit_core::{DatabaseIdentity, Engine, ResetMarker, telemetry::*};
use std::collections::BTreeMap;

fn sample(node: i16) -> NativeCounterSet {
    let name = "private clerk name";
    NativeCounterSet {
        identity: NativeIdentity::new("MEMORYCLERK_SQLBUFFERPOOL")
            .qualified("native_object", MEMORY_CLERK_NATIVE_OBJECT)
            .qualified("memory_node_id", node.to_string())
            .qualified("name_sha256", digest(name.as_bytes())),
        scope: MeasurementScope::Server,
        counters: vec![
            NativeCounterSample::gauge(
                "clerk_count",
                2,
                CounterUnit::Count,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "COUNT_BIG(*)",
            ),
            NativeCounterSample::gauge(
                "pages_kb",
                9_007_199_254_740_993,
                CounterUnit::Kibibytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "pages_kb",
            ),
            NativeCounterSample::gauge(
                "virtual_memory_reserved_kb",
                1_024,
                CounterUnit::Kibibytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "virtual_memory_reserved_kb",
            ),
            NativeCounterSample::gauge(
                "virtual_memory_committed_kb",
                512,
                CounterUnit::Kibibytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "virtual_memory_committed_kb",
            ),
            NativeCounterSample::gauge(
                "awe_allocated_kb",
                0,
                CounterUnit::Kibibytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "awe_allocated_kb",
            ),
            NativeCounterSample::gauge(
                "minimum_page_size_in_bytes",
                8_192,
                CounterUnit::Bytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "page_size_in_bytes",
            ),
            NativeCounterSample::gauge(
                "maximum_page_size_in_bytes",
                8_192,
                CounterUnit::Bytes,
                MeasurementScope::Server,
                MEMORY_CLERK_NATIVE_OBJECT,
                "page_size_in_bytes",
            ),
        ],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: BTreeMap::from([
            ("type".into(), "MEMORYCLERK_SQLBUFFERPOOL".into()),
            ("name".into(), name.into()),
            ("memory_node_id".into(), node.to_string()),
            ("aggregation".into(), "clerk-type-name-and-memory-node".into()),
        ]),
    }
}
fn context() -> NativeReadContext {
    NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::Target,
        database_writable: None,
        observed_from: "2026-11-18T12:00:00.000000001Z".into(),
        observed_until: "2026-11-18T12:00:00.000000009Z".into(),
    }
}
fn observation(sample: NativeCounterSet) -> OperationalObservation {
    OperationalObservation {
        source: culprit_core::source_id::MEMORY_CLERKS.into(),
        key: sample.key().unwrap(),
        fact: OperationalFact::CounterSet(Box::new(sample)),
    }
}
fn source(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: culprit_core::source_id::MEMORY_CLERKS.into(), state, reason: None, reason_message: None }
}
fn point(samples: &[NativeCounterSet]) -> Result<MemoryClerkReport> {
    explain(MemoryClerkEvidence::Snapshot {
        samples: &samples.iter().collect::<Vec<_>>(),
        source: Some(&source(SourceState::Collected)),
    })
}
fn field<'a>(entry: &'a MemoryClerkEntry, key: &str) -> &'a MemoryClerkField {
    entry.fields.iter().find(|field| field.key == key).unwrap()
}

#[test]
fn exact_dimensions_stay_separate_and_nullable_without_a_total_or_database_owner() {
    let rows = [observation(sample(0)), observation(sample(1))];
    let references = rows.iter().collect::<Vec<_>>();
    let context = context();
    let coverage = StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 2);
    let report = explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage: &coverage,
        observations: &references,
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.native_source, Some(coverage));
    assert_eq!(
        report.context.as_ref().unwrap().bounds(),
        (context.observed_from.as_str(), context.observed_until.as_str())
    );
    assert_eq!((report.native_rows, report.observed_rows, report.presented_rows), (Some(2), 2, 2));
    let entry = &report.entries[0];
    assert_eq!(entry.scope, MeasurementScope::Server);
    assert_eq!(entry.fields.len(), 9);
    assert_eq!(field(entry, "pages_kb").value.as_deref(), Some("9007199254740993"));
    assert_eq!(field(entry, "virtual_memory_reserved_kb").value.as_deref(), Some("1024"));
    assert_eq!(field(entry, "virtual_memory_committed_kb").value.as_deref(), Some("512"));
    assert_eq!(field(entry, "awe_allocated_kb").value.as_deref(), Some("0"));
    assert_eq!(field(entry, "shared_memory_reserved_kb").value, None);
    assert_ne!(report.entries[0].observation_key, report.entries[1].observation_key);
    assert!(report.limitations.contains(&t!("collected.memory_clerks.limit.scope")));
    assert!(report.limitations.contains(&t!("collected.memory_clerks.limit.owner")));
    assert!(!serde_json::to_string(&report).unwrap().contains("\"severity\""));
}

#[test]
fn native_visibility_and_warehouse_body_gaps_remain_independent() {
    let context = context();
    let original = StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 2);
    let report = explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage: &original,
        observations: &[],
        read_complete: false,
    })
    .unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.native_source, Some(original.clone()));
    assert_eq!((report.native_rows, report.observed_rows), (Some(2), 0));
    assert_eq!(report.read_complete, Some(false));
    assert!(
        explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage: &original,
            observations: &[],
            read_complete: true
        })
        .is_err()
    );
    for coverage in [
        StateSourceCoverage::unavailable(
            culprit_core::source_id::MEMORY_CLERKS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        StateSourceCoverage::not_applicable(culprit_core::source_id::MEMORY_CLERKS.into()),
    ] {
        let report = explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage: &coverage,
            observations: &[],
            read_complete: false,
        })
        .unwrap();
        assert_eq!(report.source_state, Some(coverage.state));
        assert_eq!(report.summary, t!("collected.memory_clerks.summary.unavailable"));
    }
    let empty = StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 0);
    let report = explain(MemoryClerkEvidence::Captured {
        context: &context,
        coverage: &empty,
        observations: &[],
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.summary, t!("collected.memory_clerks.summary.empty"));
    assert!(report.entries.is_empty());
}

#[test]
fn body_and_native_source_bindings_cannot_be_replaced() {
    let context = context();
    let coverage = StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), 1);
    let mut row = observation(sample(0));
    row.source = "another native source".into();
    assert!(
        explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage: &coverage,
            observations: &[&row],
            read_complete: true
        })
        .is_err()
    );
    row.source = coverage.source.clone();
    row.key = digest(b"a substituted row key");
    assert!(
        explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage: &coverage,
            observations: &[&row],
            read_complete: true
        })
        .is_err()
    );
    let row = observation(sample(0));
    let mut wrong = coverage.clone();
    wrong.source = "another declared source".into();
    assert!(
        explain(MemoryClerkEvidence::Captured {
            context: &context,
            coverage: &wrong,
            observations: &[&row],
            read_complete: true
        })
        .is_err()
    );
}

#[test]
fn unknown_or_contradictory_native_schema_never_becomes_an_allocation() {
    for case in [
        "scope",
        "unit",
        "nature",
        "object",
        "column",
        "instance",
        "type",
        "reset",
        "zero_count",
        "duplicate_counter",
        "extra_qualifier",
        "extra_property",
        "node",
        "name_hash",
        "page_bounds",
    ] {
        let mut value = sample(0);
        match case {
            "scope" => value.scope = MeasurementScope::Database,
            "unit" => value.counters[1].unit = CounterUnit::Bytes,
            "nature" => value.counters[1].nature = CounterNature::Cumulative,
            "object" => value.counters[1].native_object = "other view".into(),
            "column" => value.counters[1].native_name = "different column".into(),
            "instance" => value.counters[1].native_instance = Some("foreign native instance".into()),
            "type" => value.counters[1].native_type = Some("65792".into()),
            "reset" => value.counters[1].reset = ResetMarker::Generation("unrelated-reset".into()),
            "zero_count" => value.counters[0].value = MeasurementValue::Integer("0".into()),
            "duplicate_counter" => value.counters.push(value.counters[0].clone()),
            "extra_qualifier" => {
                value.identity.qualifiers.insert("invented discriminator".into(), "second group".into());
            }
            "extra_property" => {
                value.properties.insert("private property".into(), "unused secret".into());
            }
            "node" => {
                value.properties.insert("memory_node_id".into(), "65536".into());
            }
            "name_hash" => {
                value.properties.insert("name".into(), "another group".into());
            }
            "page_bounds" => value.counters.last_mut().unwrap().value = MeasurementValue::Integer("4096".into()),
            _ => unreachable!(),
        }
        assert!(point(&[value]).is_err(), "{case}");
    }
}

#[test]
fn duplicate_groups_cannot_be_split_with_an_extra_identity_qualifier() {
    let first = sample(0);
    assert!(point(&[first.clone(), first.clone()]).is_err());
    let mut duplicate = first.clone();
    duplicate.identity.qualifiers.insert("extra".into(), "not another native group".into());
    assert!(duplicate.is_valid());
    assert_ne!(first.key().unwrap(), duplicate.key().unwrap());
    assert!(point(&[first, duplicate]).is_err());
}

#[test]
fn presentation_limit_never_discards_counts_or_validation_of_the_unrendered_tail() {
    let rows = (0..=MAX_MEMORY_CLERK_ENTRIES).map(|node| observation(sample(node as i16))).collect::<Vec<_>>();
    let coverage = StateSourceCoverage::collected(culprit_core::source_id::MEMORY_CLERKS.into(), rows.len() as u32);
    let refs = rows.iter().collect::<Vec<_>>();
    let report = explain(MemoryClerkEvidence::Captured {
        context: &context(),
        coverage: &coverage,
        observations: &refs,
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.native_rows, Some(257));
    assert_eq!(report.observed_rows, 257);
    assert_eq!(report.presented_rows, 256);
    assert!(report.entries_limited);
    let mut bad = rows;
    let OperationalFact::CounterSet(last) = &mut bad.last_mut().unwrap().fact else { panic!() };
    last.counters[1].unit = CounterUnit::Bytes;
    let refs = bad.iter().collect::<Vec<_>>();
    assert!(
        explain(MemoryClerkEvidence::Captured {
            context: &context(),
            coverage: &coverage,
            observations: &refs,
            read_complete: true
        })
        .is_err()
    );
}

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap()
}
#[test]
fn snapshot_context_stays_unknown_and_bad_raw_input_produces_an_explicit_pipeline_gap() {
    let mut snapshot = snapshot();
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![sample(0)]);
    let report = explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(report.context.is_none());
    assert!(report.native_source.is_none());
    assert!(report.native_rows.is_none());
    assert!(report.read_complete.is_none());
    assert_eq!(report.entries.len(), 1);
    let pressure = snapshot.pressure.as_mut().unwrap();
    pressure.resource_counters.as_mut().unwrap()[0].counters[1].unit = CounterUnit::Bytes;
    let report = crate::analyze(snapshot);
    assert!(report.memory_clerks.is_none());
    assert!(
        report.snapshot.sources.iter().any(|source| source.source == culprit_core::source_id::MEMORY_CLERKS
            && source.state == SourceState::Unavailable)
    );
}

#[test]
fn sharing_withholds_clerk_labels_but_keeps_an_explicit_repeatable_gap() {
    let mut original = snapshot();
    original.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![sample(0)]);
    let shared = culprit_core::redact::redact_snapshot(&original);
    assert!(!serde_json::to_string(&shared).unwrap().contains("private clerk name"));
    let report = explain_snapshot(&shared).unwrap().unwrap();
    assert_eq!(report.source_state, Some(SourceState::Unavailable));
    assert_eq!(report.observed_rows, 0);
    assert!(report.entries.is_empty());
    let again = culprit_core::redact::redact_snapshot(&shared);
    let report = explain_snapshot(&again).unwrap().unwrap();
    assert_eq!(report.source_state, Some(SourceState::Unavailable));
    assert!(report.source_reason.is_some());
    assert_eq!(
        original.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap()[0].properties["name"],
        "private clerk name"
    );
}

mod captured;
