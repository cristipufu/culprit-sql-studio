use super::*;
fn context() -> NativeReadContext {
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    NativeReadContext {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(member), epoch: Some(digest(b"original-cpu")) },
        observed_from: "2020-01-01T05:59:59Z".into(),
        observed_until: "2020-01-01T06:00:01Z".into(),
        database_writable: None,
    }
}
fn sample(kind: &str, object: &str, id: &str) -> NativeCounterSet {
    let context = context();
    let CollectionTarget::Shared(member) = &context.target else { panic!() };
    NativeCounterSet {
        identity: NativeIdentity::new(id)
            .qualified("kind", kind)
            .qualified("native_object", object)
            .qualified("instance_id", member.telemetry_id().unwrap())
            .qualified("instance_epoch", context.partition.native_epoch().unwrap()),
        scope: MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: [
            ("observed_at".into(), canonical_time("2020-01-01T06:00:00Z").unwrap()),
            ("started_at_ticks".into(), "100".into()),
            ("sample_ms_ticks".into(), "500".into()),
            ("major".into(), "16".into()),
            ("engine_edition".into(), "3".into()),
        ]
        .into(),
    }
}
fn pool() -> NativeCounterSet {
    let mut sample = sample(MSSQL_CPU_POOL_KIND, MSSQL_CPU_POOL_OBJECT, "2");
    sample.properties.extend([
        ("host_logical_processors".into(), "64".into()),
        ("statistics_started_without_offset".into(), "2020-01-01T05:00:00.0030000".into()),
    ]);
    sample.counters.push(NativeCounterSample {
        key: "total_cpu_usage_ms".into(),
        native_name: "total_cpu_usage_ms".into(),
        native_object: MSSQL_CPU_POOL_OBJECT.into(),
        native_instance: Some("2".into()),
        native_type: None,
        value: MeasurementValue::Integer("9007199254740993".into()),
        unit: CounterUnit::Milliseconds,
        nature: CounterNature::Cumulative,
        scope: MeasurementScope::Server,
        reset: ResetMarker::Unknown,
    });
    sample
}
fn history(platform: &str, process: &str, idle: &str) -> NativeCounterSet {
    let mut sample = sample(MSSQL_CPU_HISTORY_KIND, MSSQL_CPU_HISTORY_OBJECT, "7");
    sample.properties.extend([
        ("host_platform".into(), platform.into()),
        ("record_ms_ticks".into(), "400".into()),
        ("process_utilization".into(), process.into()),
        ("system_idle".into(), idle.into()),
    ]);
    sample
}
fn description() -> CollectionDescription {
    let context = context();
    CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: MSSQL_CPU_PROFILE_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::ProcessCpu,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: 2,
            sources: vec![
                StateSourceCoverage::unavailable(
                    source_id::PROCESSOR_TOPOLOGY.into(),
                    crate::ErrorCode::PermissionDenied,
                    None,
                ),
                StateSourceCoverage::collected(MSSQL_CPU_POOLS_SOURCE.into(), 1),
                StateSourceCoverage::collected(MSSQL_CPU_HISTORY_SOURCE.into(), 1),
            ],
        }),
    }
}
#[test]
fn native_pool_counter_keeps_exact_ms_unknown_reset_and_all_logical_capacity_distinct() {
    let sample = pool();
    let point = MssqlCpuPoolPoint::from_original(&sample, &context()).unwrap();
    assert_eq!(point.total_ms(), Some("9007199254740993"));
    assert_eq!(point.host_processors(), "64");
    assert_eq!(point.statistics_started(), "2020-01-01T05:00:00.0030000");
    let mut zero = sample.clone();
    zero.counters[0].value = MeasurementValue::Integer("0".into());
    assert_eq!(MssqlCpuPoolPoint::from_sample(&zero).unwrap().total_ms(), Some("0"));
    zero.counters.clear();
    assert_eq!(MssqlCpuPoolPoint::from_sample(&zero).unwrap().total_ms(), None);
    let restored: NativeCounterSet = serde_json::from_slice(&serde_json::to_vec(&sample).unwrap()).unwrap();
    assert_eq!(restored, sample);
}
#[test]
fn wrong_native_unit_counter_reset_capacity_and_extra_property_refuse_without_normalization() {
    for case in 0..6 {
        let mut bad = pool();
        match case {
            0 => bad.counters[0].unit = CounterUnit::Count,
            1 => bad.counters[0].reset = ResetMarker::Generation(digest(b"invented-reset")),
            2 => {
                bad.properties.insert("host_logical_processors".into(), "0".into());
            }
            3 => {
                bad.properties.insert("statistics_started_without_offset".into(), "2020-01-01T05:00:00Z".into());
            }
            4 => {
                bad.properties.insert("other_process_cpu".into(), "50".into());
            }
            _ => bad.counters[0].value = MeasurementValue::Integer("-1".into()),
        };
        assert!(MssqlCpuPoolPoint::from_sample(&bad).is_err(), "{case}");
    }
}
#[test]
fn native_history_does_not_turn_linux_idle_or_unknown_platform_raw_fields_into_measurements() {
    for (platform, process, idle, expected_process, expected_idle) in [
        ("Windows", "10", "50", Some("10"), Some("50")),
        ("Linux", "10", "0", Some("10"), None),
        ("Unknown", "10", "0", None, None),
        ("Windows", "180", "-1", None, None),
    ] {
        let sample = history(platform, process, idle);
        let point = MssqlCpuHistoryPoint::from_original(&sample, &context()).unwrap();
        assert_eq!(point.raw_process(), Some(process));
        assert_eq!(point.raw_idle(), Some(idle));
        assert_eq!(point.process_percent(), expected_process);
        assert_eq!(point.idle_percent(), expected_idle);
        assert_eq!(point.record_ticks(), "400");
        assert!(sample.counters.is_empty());
    }
    let mut old = history("Linux", "10", "0");
    old.properties.insert("major".into(), "14".into());
    assert_eq!(MssqlCpuHistoryPoint::from_sample(&old).unwrap().process_percent(), None);
}
#[test]
fn stale_or_future_native_record_and_changed_original_process_cannot_borrow_current_owner() {
    for ticks in ["99", "501"] {
        let mut sample = history("Windows", "10", "50");
        sample.properties.insert("record_ms_ticks".into(), ticks.into());
        assert!(MssqlCpuHistoryPoint::from_sample(&sample).is_err());
    }
    let sample = pool();
    let mut changed = context();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
    *epoch = Some(digest(b"restarted"));
    assert!(MssqlCpuPoolPoint::from_original(&sample, &changed).is_err());
    let mut outside = context();
    outside.observed_until = "2020-01-01T05:59:59Z".into();
    assert!(MssqlCpuPoolPoint::from_original(&sample, &outside).is_err());
}
#[test]
fn only_three_declared_cpu_populations_and_exact_instance_scope_accept_original_records() {
    let source = description();
    assert!(cpu_profile_header_is_valid(&source));
    assert!(source.partition_is_consistent());
    let row = OperationalObservation::from_counters(MSSQL_CPU_POOLS_SOURCE, pool()).unwrap();
    assert!(cpu_profile_observation_fits(&row, &source));
    let mut wrong = source.clone();
    let CollectionTarget::Shared(target) = &mut wrong.target else { panic!() };
    target.engine = Engine::Postgres;
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut wrong.partition else { panic!() };
    member.engine = Engine::Postgres;
    assert!(!cpu_profile_header_is_valid(&wrong));
    assert!(!cpu_profile_observation_fits(&row, &wrong));
    let mut unknown = source.clone();
    unknown.partition = SourcePartition::Target;
    assert!(!cpu_profile_header_is_valid(&unknown));
    let mut extra = source.clone();
    let SourceObservation::Snapshot(header) = &mut extra.observation else { panic!() };
    header.sources.push(StateSourceCoverage::collected("foreign".into(), 0));
    assert!(!cpu_profile_header_is_valid(&extra));
}

#[test]
fn windows_other_process_cpu_uses_one_original_record_and_keeps_native_inputs_unchanged() {
    for (process, idle, expected) in
        [("10", "50", 40), ("10", "90", 0), ("0", "0", 100), ("0", "100", 0), ("100", "0", 0)]
    {
        let sample = history("Windows", process, idle);
        let original = serde_json::to_value(&sample).unwrap();
        let point = MssqlCpuHistoryPoint::from_original(&sample, &context()).unwrap();
        assert_eq!(point.other_process_percent(), Some(expected));
        assert_eq!(point.raw_process(), Some(process));
        assert_eq!(point.raw_idle(), Some(idle));
        assert_eq!(point.record_ticks(), "400");
        assert_eq!(serde_json::to_value(&sample).unwrap(), original);
        assert!(!sample.properties.contains_key("other_process_cpu_percent"));
    }
}

#[test]
fn other_process_cpu_refuses_missing_unsupported_and_incompatible_inputs_without_clamping() {
    for (platform, process, idle) in [
        ("Linux", "10", "0"),
        ("Unknown", "10", "50"),
        ("Windows", "60", "50"),
        ("Windows", "101", "0"),
        ("Windows", "0", "101"),
        ("Windows", "-1", "50"),
        ("Windows", "10", "-1"),
    ] {
        let sample = history(platform, process, idle);
        let point = MssqlCpuHistoryPoint::from_original(&sample, &context()).unwrap();
        assert_eq!(point.other_process_percent(), None, "{platform}/{process}/{idle}");
        assert_eq!(point.raw_process(), Some(process));
        assert_eq!(point.raw_idle(), Some(idle));
    }
    for missing in ["process_utilization", "system_idle", "host_platform"] {
        let mut sample = history("Windows", "10", "50");
        sample.properties.remove(missing);
        assert_eq!(MssqlCpuHistoryPoint::from_original(&sample, &context()).unwrap().other_process_percent(), None);
    }
    let mut old = history("Windows", "10", "50");
    old.properties.insert("major".into(), "13".into());
    assert_eq!(MssqlCpuHistoryPoint::from_sample(&old).unwrap().other_process_percent(), None);
    assert!(
        MssqlCpuHistoryPoint::from_original(&pool(), &context()).is_err(),
        "pool totals are a different native population"
    );
    let mut changed_owner = context();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut changed_owner.partition else { panic!() };
    *epoch = Some(digest(b"another-native-process"));
    assert!(MssqlCpuHistoryPoint::from_original(&history("Windows", "10", "50"), &changed_owner).is_err());
}
