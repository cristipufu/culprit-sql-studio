use super::*;
fn point() -> TimescaleContinuousAggregate {
    let relation = |schema: &str, name: &str| TimescaleRelationLabel { schema: schema.into(), name: name.into() };
    TimescaleContinuousAggregate {
        position: 1,
        observed_at: "2020-01-02T12:00:00.5Z".into(),
        view: relation("public", "aggregate"),
        source_hypertable: relation("public", "readings"),
        materialization_hypertable: relation("_timescaledb_internal", "_materialized_hypertable_2"),
        owner: "native-owner".into(),
        materialized_only: false,
        compression_enabled: true,
        granular_refresh_enabled: None,
    }
}
fn fixture() -> (CollectionDescription, OperationalObservation) {
    let point = point();
    let row = OperationalObservation {
        source: TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::TimescaleContinuousAggregate(Box::new(point)),
    };
    let mut source = source(&[]);
    let SourceObservation::Jobs { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 1;
    snapshot.sources[2] = StateSourceCoverage::collected(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), 1);
    (source, row)
}
#[test]
fn configuration_retains_native_nulls_and_qualified_labels_with_exact_current_source_coverage() {
    let (source, row) = fixture();
    let read = TimescaleJobsSnapshot::from_source(&source, vec![row.clone()]).unwrap();
    assert!(read.is_valid());
    let record = TelemetryRecord::Operational(row);
    let capture = CaptureIdentity { observation_id: "original-aggregate".into(), revision: None };
    let original = ExportedRecord::new(&source, &capture, record).unwrap();
    assert_eq!(serde_json::from_value::<ExportedRecord>(serde_json::to_value(&original).unwrap()).unwrap(), original);
    let OperationalFact::TimescaleContinuousAggregate(point) = &read.observations[0].fact else { panic!() };
    assert!(point.granular_refresh_enabled.is_none());
    assert!(!point.materialized_only);
}
#[test]
fn catalog_points_cannot_borrow_an_unavailable_view_other_engine_or_ordinary_operational_source() {
    let (source, row) = fixture();
    for change in ["denied", "time", "engine", "ordinary"] {
        let mut source = source.clone();
        match change {
            "denied" => {
                let SourceObservation::Jobs { metadata, .. } = &mut source.observation else { panic!() };
                metadata.views[4].state = TimescaleJobViewState::PermissionDenied;
            }
            "time" => {
                let SourceObservation::Jobs { snapshot, .. } = &mut source.observation else { panic!() };
                snapshot.observed_until = snapshot.observed_from.clone();
            }
            "engine" => {
                source.target = DatabaseIdentity::server(Engine::Mssql, "owned-db", "owned.invalid", 1433, None).into()
            }
            "ordinary" => {
                let SourceObservation::Jobs { snapshot, .. } = source.observation.clone() else { panic!() };
                source.observation = SourceObservation::Snapshot(snapshot);
            }
            _ => unreachable!(),
        }
        assert!(TimescaleJobsSnapshot::from_source(&source, vec![row.clone()]).is_err(), "{change}");
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original-aggregate".into(), revision: None },
                TelemetryRecord::Operational(row.clone())
            )
            .is_err(),
            "{change}"
        );
    }
}
#[test]
fn invalid_native_position_or_coverage_is_rejected_without_an_invented_empty_catalog() {
    for position in [0, MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS + 1] {
        let mut point = point();
        point.position = position;
        assert!(!point.is_valid());
        assert!(point.key().is_err());
    }
    let (mut source, row) = fixture();
    let SourceObservation::Jobs { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS + 1;
    snapshot.sources[2] =
        StateSourceCoverage::collected(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), snapshot.records);
    assert!(TimescaleJobsSnapshot::from_source(&source, vec![row]).is_err());
}
