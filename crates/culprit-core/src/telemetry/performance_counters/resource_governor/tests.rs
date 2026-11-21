use super::*;

fn original() -> (CollectionDescription, NativeCounterSet) {
    let owner = SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-instance-namespace"),
        native: NativeIdentity::new("native-instance-a"),
    };
    let epoch = digest(b"counter-native-process");
    let name = "name-matching-a-database";
    let source = CollectionDescription {
        target: CollectionTarget::Shared(owner.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(owner.clone()), epoch: Some(epoch.clone()) },
        source: "shared-operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected(crate::source_id::PERFORMANCE_COUNTERS.into(), 1)],
        }),
    };
    let sample = NativeCounterSet {
        identity: NativeIdentity::new(digest(name.as_bytes()))
            .qualified("kind", MSSQL_RESOURCE_GOVERNOR_COUNTER_KIND)
            .qualified("native_object", "sys.dm_os_performance_counters")
            .qualified("resource_kind", "workload-group")
            .qualified("resource_governor_instance_id", owner.telemetry_id().unwrap())
            .qualified("resource_governor_instance_epoch", epoch),
        scope: crate::MeasurementScope::Server,
        counters: vec![NativeCounterSample {
            key: "completed".into(),
            value: MeasurementValue::Integer("9007199254740993".into()),
            unit: CounterUnit::Count,
            nature: CounterNature::Cumulative,
            scope: crate::MeasurementScope::Server,
            reset: crate::ResetMarker::Unknown,
            native_object: "SQLServer:Workload Group Stats".into(),
            native_name: "Requests completed/sec".into(),
            native_instance: Some(name.into()),
            native_type: Some("272696576".into()),
        }],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: [
            (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE.into()),
            (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
            (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "1".into()),
            ("resource_name".into(), name.into()),
            ("group_id".into(), "3".into()),
            ("pool_id".into(), "2".into()),
            ("statistics_started_without_offset".into(), "2019-12-31T22:00:00.0000000".into()),
        ]
        .into(),
    };
    (source, sample)
}

#[test]
fn native_ids_and_offset_unknown_stat_clock_are_separate_from_raw_cumulative_counter_reset() {
    let (source, sample) = original();
    let profile = crate::Engine::Mssql.spec().performance_counters().unwrap();
    profile.validate(&sample).unwrap();
    assert!(resource_governor_fits_source(&sample, &source));
    let metadata = MssqlResourceGovernorCounterSet::from_sample(&sample).unwrap().context();
    assert_eq!((metadata.native_group_id.as_deref(), metadata.native_pool_id.as_deref()), (Some("3"), Some("2")));
    assert!(metadata.statistics_started_without_offset.as_deref().unwrap().ends_with(".0000000"));
    assert_eq!(sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(sample.counters[0].reset, crate::ResetMarker::Unknown);
    assert!(source.target.database().is_none());
    let restored: NativeCounterSet = serde_json::from_slice(&serde_json::to_vec(&sample).unwrap()).unwrap();
    assert_eq!(restored, sample);
}

#[test]
fn name_only_counter_is_not_promoted_to_a_native_group_or_pool_identity_or_zero() {
    let (source, mut sample) = original();
    for field in ["group_id", "pool_id", "statistics_started_without_offset"] {
        sample.properties.remove(field);
    }
    crate::Engine::Mssql.spec().performance_counters().unwrap().validate(&sample).unwrap();
    assert!(resource_governor_fits_source(&sample, &source));
    let metadata = MssqlResourceGovernorCounterSet::from_sample(&sample).unwrap();
    assert!(!metadata.has_native_ids());
    assert_eq!(metadata.context().native_group_id, None);
    assert_eq!(metadata.context().native_pool_id, None);
    assert_eq!(sample.counters[0].value.integer(), Some(9_007_199_254_740_993));
}

#[test]
fn sharing_withdraws_native_governor_identity_labels_and_statistics_clock_without_repairing_ownership() {
    let (_, sample) = original();
    let exact = sample.counters[0].value.clone();
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.sources = vec![crate::SourceOutcome {
        source: crate::source_id::PERFORMANCE_COUNTERS.into(),
        state: crate::SourceState::Collected,
        reason: None,
        reason_message: None,
    }];
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![sample]);
    let shared = crate::redact::redact_snapshot(&snapshot);
    let samples = shared.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].counters[0].value, exact);
    assert_eq!(samples[0].properties.get(PERFORMANCE_COUNTER_WITHHELD_PROPERTY).map(String::as_str), Some("true"));
    assert!(MssqlResourceGovernorCounterSet::from_sample(&samples[0]).is_err());
    let text = serde_json::to_string(samples).unwrap();
    for private in [
        "name-matching-a-database",
        "statistics_started_without_offset",
        "2019-12-31T22:00:00.0000000",
        "resource_governor_instance_id",
    ] {
        assert!(!text.contains(private), "{private}");
    }
}

#[test]
fn wrong_instance_epoch_clock_dataset_or_database_route_refuses_original_source_authority() {
    let (source, sample) = original();
    let mut wrong = source.clone();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut wrong.partition else { panic!() };
    *epoch = Some(digest(b"other-process"));
    let mut outside = source.clone();
    let SourceObservation::Snapshot(header) = &mut outside.observation else { panic!() };
    header.observed_until = "2020-01-01T12:00:00Z".into();
    let mut dataset = source.clone();
    let SourceObservation::Snapshot(header) = &mut dataset.observation else { panic!() };
    header.dataset = SnapshotDataset::MemoryFailures;
    let mut database = source.clone();
    database.target = crate::DatabaseIdentity::server(
        crate::Engine::Mssql,
        "name-matching-a-database",
        "fixture.invalid",
        1433,
        None,
    )
    .into();
    for invalid in [wrong, outside, dataset, database] {
        assert!(!resource_governor_fits_source(&sample, &invalid));
    }
}

#[test]
fn masked_ids_native_role_gauge_type_and_reset_claims_cannot_repair_counter_meaning() {
    let (_, sample) = original();
    let profile = crate::Engine::Mssql.spec().performance_counters().unwrap();
    for case in 0..5 {
        let mut invalid = sample.clone();
        match case {
            0 => {
                invalid.properties.insert("group_id".into(), "0".into());
            }
            1 => {
                invalid.identity.qualifiers.insert("resource_kind".into(), "resource-pool".into());
            }
            2 => invalid.counters[0].nature = CounterNature::Gauge,
            3 => invalid.counters[0].reset = crate::ResetMarker::Generation(digest(b"not-proven-reset")),
            4 => {
                invalid.properties.insert("statistics_started_without_offset".into(), "2019-12-31T22:00:00Z".into());
            }
            _ => unreachable!(),
        }
        assert!(profile.validate(&invalid).is_err(), "case {case}");
    }
}
