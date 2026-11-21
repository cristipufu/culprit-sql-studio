use super::*;

fn original() -> (NativeReadContext, NativeCounterSet) {
    let owner = SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    let epoch = digest(b"original-process");
    let context = NativeReadContext {
        target: CollectionTarget::Shared(owner.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(owner.clone()), epoch: Some(epoch.clone()) },
        observed_from: "2020-01-01T11:59:59Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    let sample = NativeCounterSet {
        identity: NativeIdentity::new("42")
            .qualified("kind", MSSQL_PROCESS_CPU_KIND)
            .qualified("native_object", MSSQL_PROCESS_CPU_OBJECT)
            .qualified("instance_id", owner.telemetry_id().unwrap())
            .qualified("instance_epoch", &epoch),
        scope: crate::MeasurementScope::Server,
        counters: MSSQL_PROCESS_CPU_COUNTERS
            .into_iter()
            .zip(["9007199254740993", "0"])
            .map(|(name, value)| NativeCounterSample {
                key: name.into(),
                value: MeasurementValue::Integer(value.into()),
                unit: CounterUnit::Milliseconds,
                nature: CounterNature::Cumulative,
                scope: crate::MeasurementScope::Server,
                reset: crate::ResetMarker::Generation(epoch.clone()),
                native_object: MSSQL_PROCESS_CPU_OBJECT.into(),
                native_name: name.into(),
                native_instance: None,
                native_type: None,
            })
            .collect(),
        ratios: vec![],
        ratio_gaps: vec![],
        properties: [
            ("observed_at".into(), "2020-01-01T12:00:00Z".into()),
            ("started_at_ticks".into(), "100".into()),
            ("sample_ms_ticks".into(), "500".into()),
        ]
        .into(),
    };
    (context, sample)
}

#[test]
fn exact_process_totals_zero_and_unknown_roundtrip_without_a_cpu_percentage() {
    let (context, sample) = original();
    let sample: NativeCounterSet = serde_json::from_slice(&serde_json::to_vec(&sample).unwrap()).unwrap();
    let point = MssqlProcessCpuPoint::from_original(&sample, &context).unwrap();
    assert_eq!((point.user_ms(), point.kernel_ms()), (Some("9007199254740993"), Some("0")));
    let mut unknown = sample.clone();
    unknown.counters.clear();
    let point = MssqlProcessCpuPoint::from_original(&unknown, &context).unwrap();
    assert_eq!((point.user_ms(), point.kernel_ms()), (None, None));
    assert!(sample.counters.iter().all(|value| value.unit == CounterUnit::Milliseconds));
}

#[test]
fn changed_member_process_epoch_database_scope_and_outside_clock_cannot_borrow_original_authority() {
    let (context, sample) = original();
    let mut wrong_member = context.clone();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut wrong_member.partition else { panic!() };
    member.native.id = "other-instance".into();
    let mut restarted = context.clone();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut restarted.partition else { panic!() };
    *epoch = Some(digest(b"restarted"));
    let mut old_clock = context.clone();
    old_clock.observed_until = "2020-01-01T11:59:59Z".into();
    for (name, invalid) in [("member", wrong_member), ("restart", restarted), ("clock", old_clock)] {
        assert!(MssqlProcessCpuPoint::from_original(&sample, &invalid).is_err(), "{name}");
    }
    let mut unknown = context.clone();
    unknown.partition = SourcePartition::Target;
    assert!(MssqlProcessCpuPoint::from_original(&sample, &unknown).is_err());
}

#[test]
fn wrong_units_gauge_reset_duplicate_negative_and_inverted_ticks_never_become_cpu_measurements() {
    let (_, sample) = original();
    for case in 0..7 {
        let mut bad = sample.clone();
        match case {
            0 => bad.counters[0].unit = CounterUnit::Ratio,
            1 => bad.counters[0].nature = CounterNature::Gauge,
            2 => bad.counters[0].reset = crate::ResetMarker::Unknown,
            3 => bad.counters.push(bad.counters[0].clone()),
            4 => bad.counters[0].value = MeasurementValue::Integer("-1".into()),
            5 => {
                bad.properties.insert("sample_ms_ticks".into(), "99".into());
            }
            6 => {
                bad.counters[0].key = "cpu-percent".into();
            }
            _ => unreachable!(),
        }
        assert!(MssqlProcessCpuPoint::from_sample(&bad).is_err(), "case {case}");
    }
}

#[test]
fn original_raw_export_refuses_another_source_even_with_matching_coverage_and_a_changed_process_epoch() {
    let (context, sample) = original();
    let row = OperationalObservation::from_counters(crate::source_id::PROCESSOR_TOPOLOGY, sample).unwrap();
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: "shared-operational-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: 1,
            sources: vec![StateSourceCoverage::collected(crate::source_id::PROCESSOR_TOPOLOGY.into(), 1)],
        }),
    };
    let capture = CaptureIdentity { observation_id: "original-cpu-export".into(), revision: None };
    assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row.clone())).is_ok());
    let mut foreign = source.clone();
    let SourcePartition::InstanceLocal { epoch, .. } = &mut foreign.partition else { panic!() };
    *epoch = Some(digest(b"other-native-process"));
    assert!(ExportedRecord::new(&foreign, &capture, TelemetryRecord::Operational(row.clone())).is_err());
    let mut wrong_family = source;
    let SourceObservation::Snapshot(header) = &mut wrong_family.observation else { panic!() };
    header.sources[0].source = "other-family".into();
    let mut row = row;
    row.source = "other-family".into();
    assert!(!row.is_valid());
    assert!(ExportedRecord::new(&wrong_family, &capture, TelemetryRecord::Operational(row)).is_err());
}
