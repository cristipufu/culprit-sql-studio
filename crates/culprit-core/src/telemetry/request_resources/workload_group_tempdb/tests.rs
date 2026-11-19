use super::*;

fn group() -> MssqlWorkloadGroupTempdbObservation {
    MssqlWorkloadGroupTempdbObservation {
        group_id: "7".into(),
        pool_id: "2".into(),
        statistics_started_without_offset: Some("2020-01-01T10:00:00.003".into()),
        current_kibibytes: Some("0".into()),
        peak_kibibytes: Some("9007199254740993".into()),
        limit_violations: Some("3".into()),
        stored_limits: Some(MssqlStoredTempdbDataLimits {
            fixed_mebibytes: Some("0.5".into()),
            percent: Some("10.25".into()),
        }),
    }
}
fn grant() -> ExecutionGrantObservation {
    let mut grant = super::super::tests::grant();
    grant.resource = Some(
        NativeIdentity::new("2")
            .qualified("kind", "sql-server-resource-governor")
            .qualified("resource_semaphore_id", "0"),
    );
    grant.workload_group = Some(NativeIdentity::new("7").qualified("kind", "sql-server-resource-governor-group"));
    grant.workload_group_tempdb = Some(group());
    grant
}
fn context() -> NativeReadContext {
    let mut context = super::super::tests::context(&grant().observed_at);
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { unreachable!() };
    member.kind = SharedResourceKind::Instance;
    context
}
fn source(context: NativeReadContext) -> CollectionDescription {
    CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: crate::source_id::CURRENT_REQUEST_MEMORY.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::CurrentRequestMemory,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: 1,
            sources: vec![StateSourceCoverage::collected(crate::source_id::CURRENT_REQUEST_MEMORY.into(), 1)],
        }),
    }
}

#[test]
fn shared_group_data_current_peak_stored_limits_and_abort_history_keep_distinct_native_meanings() {
    let mut grant = grant();
    assert!(grant.is_valid() && grant.fits_context(&context()));
    let key = grant.key().unwrap();
    let restored: ExecutionGrantObservation = serde_json::from_slice(&serde_json::to_vec(&grant).unwrap()).unwrap();
    assert_eq!(restored, grant);
    let group = restored.workload_group_tempdb.as_ref().unwrap();
    assert_eq!(group.current_kibibytes.as_deref(), Some("0"));
    assert_eq!(group.peak_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(group.limit_violations.as_deref(), Some("3"));
    assert_eq!(group.stored_limits.as_ref().unwrap().fixed_mebibytes.as_deref(), Some("0.5"));
    let observed = grant.workload_group_tempdb.as_mut().unwrap();
    observed.current_kibibytes = Some("128".into());
    observed.peak_kibibytes = None;
    observed.limit_violations = Some("0".into());
    observed.statistics_started_without_offset = Some("2020-02-01T12:00:00.003".into());
    assert_eq!(
        grant.key().unwrap(),
        key,
        "mutable group statistics/reset context cannot become a grant or group incarnation"
    );
}

#[test]
fn known_group_and_pool_binding_rejects_masked_foreign_and_unqualified_identifiers() {
    for invalid in [
        "group",
        "pool",
        "masked-group",
        "masked-pool",
        "group-kind",
        "pool-kind",
        "negative",
        "fractional-count",
        "utc-reset",
        "invalid-percent",
        "infinite-limit",
    ] {
        let mut grant = grant();
        let group = grant.workload_group_tempdb.as_mut().unwrap();
        match invalid {
            "group" => group.group_id = "8".into(),
            "pool" => group.pool_id = "3".into(),
            "masked-group" => group.group_id = "0".into(),
            "masked-pool" => group.pool_id = "0".into(),
            "group-kind" => grant.workload_group.as_mut().unwrap().qualifiers.clear(),
            "pool-kind" => grant.resource.as_mut().unwrap().qualifiers.clear(),
            "negative" => group.current_kibibytes = Some("-1".into()),
            "fractional-count" => group.limit_violations = Some("0.5".into()),
            "utc-reset" => group.statistics_started_without_offset = Some("2020-01-01T10:00:00Z".into()),
            "invalid-percent" => group.stored_limits.as_mut().unwrap().percent = Some("100.01".into()),
            _ => group.stored_limits.as_mut().unwrap().fixed_mebibytes = Some("inf".into()),
        }
        assert!(!grant.is_valid(), "{invalid}");
    }
}

#[test]
fn unavailable_layout_native_null_no_stored_limit_and_zero_setting_are_not_interchangeable() {
    let mut grant = grant();
    let group = grant.workload_group_tempdb.as_mut().unwrap();
    group.statistics_started_without_offset = None;
    group.current_kibibytes = None;
    group.peak_kibibytes = None;
    group.limit_violations = None;
    group.stored_limits = None;
    assert!(grant.is_valid());
    let unavailable = serde_json::to_value(&grant).unwrap();
    grant.workload_group_tempdb.as_mut().unwrap().stored_limits =
        Some(MssqlStoredTempdbDataLimits { fixed_mebibytes: None, percent: None });
    assert!(grant.is_valid());
    assert_ne!(serde_json::to_value(&grant).unwrap(), unavailable);
    grant.workload_group_tempdb.as_mut().unwrap().stored_limits.as_mut().unwrap().fixed_mebibytes = Some("0".into());
    assert!(grant.is_valid());
    let mut encoded = serde_json::to_value(&grant).unwrap();
    encoded.as_object_mut().unwrap().remove("workload_group_tempdb");
    assert!(serde_json::from_value::<ExecutionGrantObservation>(encoded).is_err());
}

#[test]
fn raw_original_admission_requires_the_same_known_mssql_instance_scope_and_native_read_clock() {
    let grant = grant();
    let original = context();
    let capture = CaptureIdentity { observation_id: "original-tempdb-group".into(), revision: None };
    let row = TelemetryRecord::Operational(OperationalObservation {
        source: grant.source.clone(),
        key: grant.key().unwrap(),
        fact: OperationalFact::ExecutionGrant(Box::new(grant.clone())),
    });
    assert!(ExportedRecord::new(&source(original.clone()), &capture, row.clone()).is_ok());
    for invalid in ["node", "member", "epoch", "engine", "database", "clock", "dataset"] {
        let mut context = original.clone();
        match invalid {
            "node" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else {
                    unreachable!()
                };
                member.kind = SharedResourceKind::Node;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } = &mut context.partition else { unreachable!() };
                *member = None;
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut context.partition else { unreachable!() };
                *epoch = None;
            }
            "engine" => {
                context.target =
                    DatabaseIdentity::server(crate::Engine::Postgres, "sample", "fixture.invalid", 5432, None).into();
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else {
                    unreachable!()
                };
                member.engine = crate::Engine::Postgres;
            }
            "database" => {
                context.target =
                    DatabaseIdentity::server(crate::Engine::Mssql, "other-database", "fixture.invalid", 1433, None)
                        .into();
            }
            "clock" => {
                context.observed_from = "2020-01-01T08:02:00Z".into();
                context.observed_until = context.observed_from.clone();
            }
            _ => {}
        }
        let mut source = source(context);
        if invalid == "dataset" {
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            snapshot.dataset = SnapshotDataset::Operational;
        }
        assert!(ExportedRecord::new(&source, &capture, row.clone()).is_err(), "{invalid}");
    }
}
