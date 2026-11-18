use super::*;

/// These are independent current configuration points, not historical utilization or a current limiter sample.
pub fn configured_log_rate_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let point = ConfiguredLogRateLimits {
        observed_at: Some(canonical_time("2020-01-01T12:00:00Z").unwrap()),
        database_id: "7".into(),
        database_incarnation: "native-logical-database-guid".into(),
        workload_group_id: Some("12".into()),
        workload_group_bytes_per_second: Some("9007199254740993".into()),
        resource_pool_bytes_per_second: Some("0".into()),
        instance_bytes_per_second: Some("9223372036854775807".into()),
    };
    let mut unknown = point.clone();
    unknown.workload_group_id = None;
    unknown.workload_group_bytes_per_second = None;
    unknown.resource_pool_bytes_per_second = None;
    unknown.instance_bytes_per_second = None;
    let mut mixed = point.clone();
    mixed.workload_group_bytes_per_second = Some("0".into());
    mixed.resource_pool_bytes_per_second = None;
    let mut clockless = mixed.clone();
    clockless.observed_at = None;
    [Some(point), Some(mixed), Some(unknown), None, Some(clockless)].into_iter().map(capture).collect()
}

fn capture(point: Option<ConfiguredLogRateLimits>) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut records = point
        .map(|point| {
            TelemetryRecord::Operational(OperationalObservation {
                source: CONFIGURED_LOG_RATES_SOURCE.into(),
                key: point.key().unwrap(),
                fact: OperationalFact::ConfiguredLogRateLimits(Box::new(point)),
            })
        })
        .into_iter()
        .collect::<Vec<_>>();
    let configured = if records.is_empty() {
        StateSourceCoverage::unavailable(
            CONFIGURED_LOG_RATES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(CONFIGURED_LOG_RATES_SOURCE.into(), 1, false)
    };
    let unrelated = OperationalObservation::from_counters(
        "independent-native-counters",
        NativeCounterSet {
            identity: NativeIdentity::new("unrelated"),
            scope: culprit_core::MeasurementScope::Server,
            counters: vec![NativeCounterSample::gauge(
                "unrelated",
                0,
                CounterUnit::Count,
                culprit_core::MeasurementScope::Server,
                "native",
                "unrelated",
            )],
            properties: Default::default(),
            ratios: vec![],
            ratio_gaps: vec![],
        },
    )
    .unwrap();
    records.push(TelemetryRecord::Operational(unrelated));
    let mut database =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "app", "fixture.invalid", 1433, None);
    database.incarnation = Some("native-logical-database-guid".into());
    let source = CollectionDescription {
        target: database.into(),
        partition: SourcePartition::Target,
        source: "operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![configured, StateSourceCoverage::collected("independent-native-counters".into(), 1)],
        }),
    };
    assert!(source.partition_is_consistent());
    (source, records)
}
