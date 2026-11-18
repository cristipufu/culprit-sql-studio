use super::*;
mod captured;
fn read() -> HyperscaleLogComponentRead {
    let point: HyperscaleLogComponent = serde_json::from_str(include_str!(
        "../../../../culprit-core/src/telemetry/hyperscale_log_components/tests/component.json"
    ))
    .unwrap();
    let mut database =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "app", "fixture.invalid", 1433, None);
    database.incarnation = Some(point.database_incarnation.clone());
    let row = OperationalObservation {
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::HyperscaleLogComponent(Box::new(point)),
    };
    let source = CollectionDescription {
        target: database.into(),
        partition: SourcePartition::Target,
        source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::HyperscaleLogComponents,
            observed_from: "2026-11-18T12:00:00.000000001Z".into(),
            observed_until: "2026-11-18T12:00:00.000000009Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), 1, false)],
        }),
    };
    HyperscaleLogComponentRead::from_source(&source, vec![row], true).unwrap()
}
#[test]
fn large_native_values_zero_and_report_clock_are_not_rounded_or_converted_to_acquisition_time() {
    let read = read();
    let report = explain(&read).unwrap();
    assert_eq!(report.entries[0].current_max_log_rate_bps, "9007199254740993");
    assert_eq!(report.entries[0].catchup_rate_bps, "0");
    assert_eq!(report.entries[0].catchup_distance_bytes, "9223372036854775807");
    assert_eq!(report.entries[0].last_reported_time_without_offset.as_deref(), Some("2026-11-18T12:00:00.003"));
    assert!(report.context.database_writable.is_none());
    assert_eq!(report.observed_rows, 1);
}
#[test]
fn warehouse_body_gap_and_native_failure_are_independent_and_never_healthy_empty() {
    let mut read = read();
    read.read_complete = false;
    read.observations.clear();
    let report = explain(&read).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert_eq!(report.source_state, SourceState::Partial);
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.records = 0;
    header.sources[0] = StateSourceCoverage::unavailable(
        HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert!(!report.entries_limited);
}
#[test]
fn invalid_component_tail_and_duplicate_ids_cannot_hide_behind_presentation_limit_or_roles() {
    let mut read = read();
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.records = 257;
    header.sources[0].records = 257;
    let original = read.observations[0].clone();
    read.observations = (0..257)
        .map(|index| {
            let mut row = original.clone();
            let OperationalFact::HyperscaleLogComponent(point) = &mut row.fact else { panic!() };
            point.component_id = format!("{index:08x}-2222-3333-4444-555555555555");
            row.key = point.key().unwrap();
            row
        })
        .collect();
    let report = explain(&read).unwrap();
    assert_eq!(report.entries.len(), 256);
    assert_eq!(report.native_rows, 257);
    assert_eq!(report.observed_rows, 257);
    assert!(report.entries_limited);
    read.observations[256] = read.observations[0].clone();
    assert!(explain(&read).is_err());
}
#[test]
fn unknown_native_role_and_null_metadata_remain_visible_without_role_or_cause_invention() {
    let mut read = read();
    let OperationalFact::HyperscaleLogComponent(point) = &mut read.observations[0].fact else { panic!() };
    point.role = 255;
    point.role_desc = None;
    point.replica_id = None;
    point.last_reported_time_without_offset = None;
    let report = explain(&read).unwrap();
    assert_eq!(report.entries[0].role, 255);
    assert!(report.entries[0].replica_id.is_none());
    assert!(report.entries[0].last_reported_time_without_offset.is_none());
    assert_eq!(report.entries[0].title, t!("view.hyperscale_log_components.role.unrecognized"));
}
