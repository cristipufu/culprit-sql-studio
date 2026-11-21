use super::*;
fn point() -> MariadbGaleraPoint {
    MariadbGaleraPoint {
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        available_variables: MARIADB_GALERA_FIELDS.map(str::to_owned).into(),
        ready: Some(true),
        connected: Some(true),
        component_status: Some("Primary".into()),
        local_state: Some("4".into()),
        local_state_comment: Some("Synced".into()),
        cluster_size: Some("3".into()),
        cluster_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        local_state_uuid: Some("aaaaaaaa-0000-4000-8000-000000000001".into()),
        node_uuid: Some("bbbbbbbb-0000-4000-8000-000000000002".into()),
        receive_queue: Some("2".into()),
        send_queue: Some("3".into()),
        certification_failures: Some("9007199254740993".into()),
        applier_aborts: Some("7".into()),
        replays: Some("11".into()),
        local_commits: Some("18446744073709551615".into()),
        paused_fraction: Some("0.125000".into()),
        paused_nanoseconds: Some("123456789".into()),
    }
}
fn source() -> CollectionDescription {
    let instance = SharedResourceIdentity {
        engine: culprit_core::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mariadb-server-uid"),
        native: NativeIdentity::new("iFL88RG7XPipmQ5HOnA9iLqJw6w="),
    };
    CollectionDescription {
        target: CollectionTarget::Shared(instance.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(instance), epoch: Some("original-process".into()) },
        source: MARIADB_GALERA_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Galera {
            metadata: Box::new(MariadbGaleraMetadata {
                observed_at: "2020-01-01T00:00:00Z".into(),
                enabled: Some(true),
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Galera,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(MARIADB_GALERA_SOURCE.into(), 1)],
            },
        },
    }
}
fn record(point: MariadbGaleraPoint) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation {
        source: MARIADB_GALERA_SOURCE.into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MariadbGalera(Box::new(point)),
    })
}

fn report(source: &CollectionDescription, rows: &[TelemetryRecord], complete: bool) -> Result<GaleraReport> {
    let rows = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect::<Vec<_>>();
    explain(GaleraEvidence { source, observations: &rows, read_complete: complete })
}
#[test]
fn current_queues_and_wide_independent_totals_remain_visible_without_rate_or_cause() {
    let source = source();
    let report = report(&source, &[record(point())], true).unwrap();
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.native_rows, Some(1));
    assert_eq!(report.entries[0].scope, MeasurementScope::Server);
    let entry = &report.entries[0];
    assert_eq!(entry.groups[0].fields[0].value.as_deref(), Some("2"));
    assert_eq!(entry.groups[1].fields[0].value.as_deref(), Some(format::digits("9007199254740993".into()).as_str()));
    assert_eq!(
        entry.groups[1].fields[3].value.as_deref(),
        Some(format::digits("18446744073709551615".into()).as_str())
    );
    assert_eq!(entry.groups[1].fields[5].value.as_deref(), Some(format::digits("0.125000".into()).as_str()));
    assert!(entry.session.is_none());
    assert!(report.original.as_ref().unwrap().context.target.database().is_none());
    assert!(report.limitations.contains(&t!("view.galera.reset")));
    assert!(report.limitations.contains(&t!("view.galera.non_atomic")));
}
#[test]
fn native_null_absent_and_known_zero_are_distinct_without_health_from_zero() {
    let mut point = point();
    point.receive_queue = Some("0".into());
    point.certification_failures = None;
    point.available_variables.retain(|name| name != "wsrep_local_send_queue");
    point.send_queue = None;
    let mut source = source();
    let SourceObservation::Galera { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed(MARIADB_GALERA_SOURCE.into(), 1, false);
    let report = report(&source, &[record(point)], true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.groups[0].fields[0].value.as_deref(), Some("0"));
    assert_eq!(entry.groups[0].fields[1].value.as_deref(), Some(t!("view.galera.variable_absent").as_str()));
    assert_eq!(entry.groups[1].fields[0].value, None);
    assert_eq!(entry.explanation, t!("view.galera.node_context"));
    assert_eq!(report.sources[0].state, SourceState::Partial);
    assert!(report.limitations.contains(&t!("view.galera.partial")));
}
#[test]
fn disabled_or_denied_preserves_native_metadata_without_a_healthy_empty_or_rows() {
    for denied in [false, true] {
        let mut source = source();
        let SourceObservation::Galera { snapshot, metadata } = &mut source.observation else { panic!() };
        snapshot.records = 0;
        metadata.enabled = Some(denied);
        snapshot.sources[0] = if denied {
            StateSourceCoverage::unavailable(
                MARIADB_GALERA_SOURCE.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )
        } else {
            StateSourceCoverage {
                source: MARIADB_GALERA_SOURCE.into(),
                state: SourceState::NotApplicable,
                records: 0,
                complete_scope: false,
                error_code: None,
                stopped: None,
            }
        };
        let result = report(&source, &[], true).unwrap();
        assert!(result.entries.is_empty());
        assert!(result.original.is_some());
        assert_ne!(result.sources[0].state, SourceState::Empty);
        assert_eq!(result.summary, if denied { t!("view.galera.no_status") } else { t!("view.galera.disabled") });
        assert!(report(&source, &[record(point())], false).is_err());
    }
}
#[test]
fn partial_missing_rows_remain_unknown_and_duplicate_or_foreign_originals_cannot_borrow_authority() {
    let source = source();
    let incomplete = report(&source, &[], false).unwrap();
    assert!(incomplete.entries.is_empty());
    assert_eq!(incomplete.native_rows, Some(1));
    assert_eq!(incomplete.sources[0].state, SourceState::Partial);
    assert!(report(&source, &[], true).is_err());
    assert!(report(&source, &[record(point()), record(point())], false).is_err());
    let mut foreign = source.clone();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut foreign.partition else { panic!() };
    member.native.id = "other-member".into();
    assert!(report(&foreign, &[record(point())], true).is_err());
    let mut clock = point();
    clock.observed_until = "2020-01-01T00:00:02Z".into();
    assert!(report(&source, &[record(clock)], true).is_err());
}
