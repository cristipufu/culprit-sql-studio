use super::*;
/// Controlled native component reports; these fixtures are not native execution/cost qualification.
pub fn hyperscale_log_component_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let point: HyperscaleLogComponent = serde_json::from_str(include_str!(
        "../../../../culprit-core/src/telemetry/hyperscale_log_components/tests/component.json"
    ))
    .unwrap();
    let mut storage = point.clone();
    storage.component_id = "00000002-2222-3333-4444-555555555555".into();
    storage.replica_id = None;
    storage.file_id = Some("1".into());
    storage.role = 1;
    storage.role_desc = None;
    storage.last_reported_time_without_offset = None;
    let mut unknown = point.clone();
    unknown.role = 255;
    unknown.current_max_log_rate_bps = "0".into();
    unknown.catchup_distance_bytes = "0".into();
    [
        (vec![point.clone(), storage], None),
        (vec![unknown], None),
        (vec![], Some(culprit_core::SourceState::Empty)),
        (vec![point], Some(culprit_core::SourceState::Partial)),
        (vec![], Some(culprit_core::SourceState::Unavailable)),
    ]
    .into_iter()
    .map(|(points, state)| {
        let mut database =
            culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "app", "fixture.invalid", 1433, None);
        database.incarnation = Some("native-hs-logical-database".into());
        let records = points
            .into_iter()
            .map(|point| {
                TelemetryRecord::Operational(OperationalObservation {
                    source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
                    key: point.key().unwrap(),
                    fact: OperationalFact::HyperscaleLogComponent(Box::new(point)),
                })
            })
            .collect::<Vec<_>>();
        let coverage = match state {
            Some(culprit_core::SourceState::Unavailable) => StateSourceCoverage::unavailable(
                HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
            Some(culprit_core::SourceState::Partial) => {
                StateSourceCoverage::observed(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), records.len() as u32, false)
            }
            _ => StateSourceCoverage::collected(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), records.len() as u32),
        };
        let source = CollectionDescription {
            target: database.into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mssql,
                    kind: SharedResourceKind::Node,
                    namespace: NativeIdentity::new("native-hs-logical-database")
                        .qualified("kind", "azure-sql-logical-database"),
                    native: NativeIdentity::new("11111111-2222-3333-4444-555555555555")
                        .qualified("kind", "azure-sql-hyperscale-replica"),
                }),
                epoch: Some("same-read-native-process-epoch".into()),
            },
            source: HYPERSCALE_LOG_COMPONENTS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::HyperscaleLogComponents,
                observed_from: "2026-11-18T12:00:00.000000001Z".into(),
                observed_until: "2026-11-18T12:00:00.000000009Z".into(),
                records: records.len() as u32,
                sources: vec![coverage],
            }),
        };
        assert!(source.partition_is_consistent());
        (source, records)
    })
    .collect()
}
