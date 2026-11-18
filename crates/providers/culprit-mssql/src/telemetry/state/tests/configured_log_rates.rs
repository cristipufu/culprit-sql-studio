use super::*;

fn configured_identity() -> Rec {
    let original = identity();
    let mut fields = original.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("configured_log_database_id", Val::Int(7)),
        ("configured_log_group_id", Val::Int(12)),
        ("configured_log_workload_group", Val::Int(9_007_199_254_740_993)),
        ("configured_log_resource_pool", Val::Int(0)),
        ("configured_log_instance", Val::Null),
        ("configured_log_observed_at", Val::Text("2020-01-01T06:01:59Z".into())),
    ]);
    Rec::of(&fields)
}

#[tokio::test]
async fn governance_configuration_is_a_separate_point_in_the_existing_identity_query_and_close_owner() {
    let mut source = source_initial(true, false, configured_identity(), identity());
    let description = source.describe().await.unwrap();
    let header = description.observation.snapshot().unwrap();
    let coverage = header.sources.iter().find(|source| source.source == CONFIGURED_LOG_RATES_SOURCE).unwrap();
    assert_eq!(coverage.records, 1);
    assert!(!coverage.complete_scope);
    assert_eq!(header.observed_from, canonical_time("2020-01-01T06:01:59Z").unwrap());
    let before_finish = source.query_count();
    let mut cursor = None;
    let mut points = Vec::new();
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        points.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(points.len(), 11);
    let capture = CaptureIdentity { observation_id: "current-native-governance".into(), revision: None };
    let exported =
        points.into_iter().map(|row| ExportedRecord::new(&description, &capture, row).unwrap()).collect::<Vec<_>>();
    let point = exported
        .iter()
        .find_map(|row| match &row.record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ConfiguredLogRateLimits(point),
                ..
            }) => Some(point),
            _ => None,
        })
        .unwrap();
    assert_eq!(point.workload_group_bytes_per_second.as_deref(), Some("9007199254740993"));
    assert_eq!(point.resource_pool_bytes_per_second.as_deref(), Some("0"));
    assert!(point.instance_bytes_per_second.is_none());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), before_finish + 1);
    assert_eq!(
        source
            .io
            .calls
            .iter()
            .filter(|call| call.sql.contains("primary_max_log_rate AS configured_log_workload_group"))
            .count(),
        1
    );
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn malformed_optional_configuration_does_not_erase_other_operational_facts_or_retry_native_metadata() {
    let initial = replace(&configured_identity(), &[("configured_log_instance", Val::Int(-1))]);
    let mut source = source_initial(true, false, initial, identity());
    let description = source.describe().await.unwrap();
    let header = description.observation.snapshot().unwrap();
    assert_eq!(header.records, 10);
    let coverage = header.sources.iter().find(|source| source.source == CONFIGURED_LOG_RATES_SOURCE).unwrap();
    assert_eq!(coverage.state, SourceState::Unavailable);
    assert_eq!(coverage.records, 0);
    assert!(!source.rows.iter().any(|row| matches!(
        row,
        TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ConfiguredLogRateLimits(_), .. })
    )));
    assert_eq!(
        source
            .io
            .calls
            .iter()
            .filter(|call| call.sql.contains("primary_max_log_rate AS configured_log_workload_group"))
            .count(),
        1
    );
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
