use super::*;

/// Instrumented session bytes are scoped to an instance. A separately observed event is context only; its database
/// label does not identify where allocations belong. Known zero, NULL and disabled tracking remain distinct.
pub fn session_memory_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (mut manifest, _) = fixture();
    let source_id = culprit_core::source_id::SESSION_MEMORY;
    manifest.source.target = CollectionTarget::Shared(SharedResourceIdentity {
        engine: culprit_core::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-mysql-instance").qualified("network", "fixture-network"),
        native: NativeIdentity::new("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
    });
    manifest.source.source = "session-allocation-capture".into();
    let first = SessionMemoryObservation {
        source: source_id.into(),
        identity: NativeIdentity::new("61")
            .qualified("server_uuid", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
            .qualified("server_started_second", "1577836800"),
        observed_at: "2020-01-01T12:00:00.250000000Z".into(),
        session: NativeActivityIdentity { id: "17".into(), generation: "61".into() },
        tracking_enabled: true,
        current_bytes: Some("18446744073709551615".into()),
        controlled_current_bytes: Some("0".into()),
        high_water_bytes: Some("18446744073709551615".into()),
        controlled_high_water_bytes: None,
        current_context: Some(SessionMemoryContext {
            request: NativeActivityIdentity { id: "42".into(), generation: "9007199254740993".into() },
            observed_at: "2020-01-01T12:00:00.750000000Z".into(),
            database: Some("context-only-other-db".into()),
            query_hash: Some("context-only-native-digest".into()),
        }),
        native_properties: [
            ("global_instrumentation".into(), "YES".into()),
            ("thread_instrumentation".into(), "YES".into()),
            ("INSTRUMENTED".into(), "YES".into()),
        ]
        .into(),
    };
    let mut second = first.clone();
    second.identity.id = "62".into();
    second.session = NativeActivityIdentity { id: "18".into(), generation: "62".into() };
    second.observed_at = "2020-01-01T12:00:00.500000000Z".into();
    second.current_bytes = Some("0".into());
    second.controlled_current_bytes = None;
    second.high_water_bytes = Some("9007199254740993".into());
    second.controlled_high_water_bytes = Some("0".into());
    second.current_context = None;
    let mut disabled = first.clone();
    disabled.identity.id = "63".into();
    disabled.session = NativeActivityIdentity { id: "19".into(), generation: "63".into() };
    disabled.tracking_enabled = false;
    disabled.current_bytes = None;
    disabled.controlled_current_bytes = None;
    disabled.high_water_bytes = None;
    disabled.controlled_high_water_bytes = None;
    disabled.native_properties.insert("INSTRUMENTED".into(), "NO".into());
    let full = vec![record(first), record(second)];
    let mut partial = full.clone();
    partial.push(record(disabled));
    [
        (full, culprit_core::SourceState::Collected),
        (partial, culprit_core::SourceState::Partial),
        (vec![], culprit_core::SourceState::Empty),
        (vec![], culprit_core::SourceState::Unavailable),
    ]
    .into_iter()
    .map(|(rows, state)| {
        let mut source = manifest.source.clone();
        source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::SessionMemory,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: rows.len() as u32,
            sources: vec![if state == culprit_core::SourceState::Unavailable {
                StateSourceCoverage::unavailable(source_id.into(), culprit_core::ErrorCode::PermissionDenied, None)
            } else {
                StateSourceCoverage::observed(
                    source_id.into(),
                    rows.len() as u32,
                    state != culprit_core::SourceState::Partial,
                )
            }],
        });
        (source, rows)
    })
    .collect()
}

fn record(observation: SessionMemoryObservation) -> TelemetryRecord {
    assert!(observation.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: observation.source.clone(),
        key: observation.key().unwrap(),
        fact: OperationalFact::SessionMemory(Box::new(observation)),
    })
}
