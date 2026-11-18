use super::*;
use culprit_core::telemetry::*;

fn snapshot(total: u64, used: u64) -> Snapshot {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/storage.native.json")))
            .unwrap();
    let mut native: OperationalSpace = serde_json::from_value(fixture["space"].clone()).unwrap();
    native.total_bytes = Some(total.to_string());
    native.used_bytes = Some(used.to_string());
    native.growth_enabled = Some(false);
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.server.database = "sample".into();
    snapshot.sources.clear();
    snapshot.operations = Default::default();
    snapshot.operations.spaces.push(SpaceUsage {
        origin: Some(StoragePointRef {
            source: culprit_core::source_id::FILE_CAPACITY.into(),
            key: "filegroup:1".into(),
        }),
        name: "native allocation".into(),
        kind: native.kind,
        scope: native.scope,
        total_bytes: total as f64,
        used_bytes: used as f64,
        parts: Vec::new(),
        growth_enabled: native.growth_enabled,
        limit_bytes: None,
        reuse_wait: None,
    });
    snapshot.operations.storage = Some(Box::new(StorageRead {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::collected(culprit_core::source_id::FILE_CAPACITY.into(), 1)],
        observations: vec![OperationalObservation {
            source: culprit_core::source_id::FILE_CAPACITY.into(),
            key: "filegroup:1".into(),
            fact: OperationalFact::Space(native),
        }],
        read_complete: true,
    }));
    snapshot
}

#[test]
fn exact_native_thresholds_do_not_round_large_allocations_into_a_warning_or_attention() {
    const TOTAL: u64 = 900_719_925_474_099_200;
    let warning = TOTAL / 100 * u64::from(CAPACITY_WARNING_PERCENT);
    let attention = TOTAL / 100 * u64::from(CAPACITY_ATTENTION_PERCENT);
    assert_eq!((warning - 1) as f64, warning as f64, "the chart projection cannot distinguish this boundary");
    assert!(full_spaces(&snapshot(TOTAL, warning - 1)).is_empty());
    assert_eq!(full_spaces(&snapshot(TOTAL, warning)).len(), 1);
    assert!(!full_spaces(&snapshot(TOTAL, attention - 1))[0].actionable);
    assert!(full_spaces(&snapshot(TOTAL, attention))[0].actionable);
}

#[test]
fn exact_shared_tempdb_occupancy_remains_information_for_the_shared_population() {
    let mut snapshot = snapshot(1000, 999);
    let projected = &mut snapshot.operations.spaces[0];
    projected.kind = FileKind::Temp;
    projected.scope = FileScope::Server;
    projected.origin =
        Some(StoragePointRef { source: culprit_core::source_id::TEMP_CAPACITY.into(), key: "shared-tempdb".into() });
    let read = snapshot.operations.storage.as_mut().unwrap();
    read.sources[0].source = culprit_core::source_id::TEMP_CAPACITY.into();
    let row = &mut read.observations[0];
    row.source = culprit_core::source_id::TEMP_CAPACITY.into();
    row.key = "shared-tempdb".into();
    let OperationalFact::Space(native) = &mut row.fact else { panic!() };
    native.kind = FileKind::Temp;
    native.scope = FileScope::Server;
    native.population = StoragePopulation::SharedTempdbDataFiles;
    native.database = None;
    let cases = full_spaces(&snapshot);
    assert_eq!(cases.len(), 1);
    assert!(cases[0].shared);
    assert!(!cases[0].actionable);
}

#[test]
fn missing_foreign_or_conflicting_originals_are_explicit_gaps_and_cannot_reuse_chart_numbers() {
    for change in ["missing", "foreign", "scope", "projection"] {
        let mut snapshot = snapshot(1000, 999);
        match change {
            "missing" => snapshot.operations.storage = None,
            "foreign" => snapshot.operations.spaces[0].origin.as_mut().unwrap().key = "filegroup:other".into(),
            "scope" => snapshot.operations.spaces[0].scope = FileScope::Server,
            _ => snapshot.operations.spaces[0].used_bytes = 1.0,
        }
        assert!(full_spaces(&snapshot).is_empty(), "{change}");
        let report = crate::analyze(snapshot);
        assert!(!report.findings.iter().any(|finding| finding.check == "storage-capacity"), "{change}");
        assert!(
            report.snapshot.sources.iter().any(|source| {
                source.source == culprit_core::source_id::FILE_CAPACITY
                    && source.state == culprit_core::SourceState::Unavailable
            }),
            "{change}"
        );
    }
}
