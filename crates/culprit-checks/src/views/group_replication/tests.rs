use super::*;
fn point() -> MysqlGroupReplicationPoint {
    MysqlGroupReplicationPoint::Statistics(Box::new(MysqlGroupStatistics {
        group_id: "group-native".into(),
        member_id: "remote-member".into(),
        channel: "group_replication_applier".into(),
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        view_id: Some("view-original".into()),
        certification_queue: Some("0".into()),
        checked: Some("18446744073709551615".into()),
        conflicts: Some("9007199254740993".into()),
        rows_validating: None,
        applier_queue: Some("3".into()),
        remote_applied: None,
        local_proposed: Some("17".into()),
        local_rollback: Some("1".into()),
    }))
}
fn source_with_statistics() -> CollectionDescription {
    let member = SharedResourceIdentity {
        engine: culprit_core::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("mysql-server"),
        native: NativeIdentity::new("original-member"),
    };
    CollectionDescription {
        target: CollectionTarget::Shared(member.clone()),
        partition: SourcePartition::InstanceLocal { member: Some(member), epoch: Some("original-process".into()) },
        source: MYSQL_GROUP_REPLICATION_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::GroupReplication {
            metadata: Box::new(MysqlGroupReplicationMetadata {
                observed_at: "2020-01-01T00:00:00Z".into(),
                group_id: Some("group-native".into()),
                plugin_status: Some("ACTIVE".into()),
                members_layout: true,
                statistics_layout: true,
            }),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::GroupReplication,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![
                    StateSourceCoverage::observed(MYSQL_GROUP_MEMBERS_SOURCE.into(), 0, true),
                    StateSourceCoverage::collected(MYSQL_GROUP_STATISTICS_SOURCE.into(), 1),
                ],
            },
        },
    }
}

fn original() -> (CollectionDescription, Vec<OperationalObservation>) {
    let stats = point();
    let member = MysqlGroupReplicationPoint::Member(Box::new(MysqlGroupMember {
        group_id: stats.group_id().into(),
        member_id: stats.member_id().into(),
        channel: stats.channel().into(),
        observed_at: "2020-01-01T00:00:00.4Z".into(),
        state: "ONLINE".into(),
        role: Some("SECONDARY".into()),
        version: Some("8.4.7".into()),
    }));
    let rows = [member, stats]
        .into_iter()
        .map(|point| OperationalObservation {
            source: point.source().into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MysqlGroupReplication(Box::new(point)),
        })
        .collect();
    let mut source = source_with_statistics();
    let SourceObservation::GroupReplication { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 2;
    snapshot.sources[0] = StateSourceCoverage::collected(MYSQL_GROUP_MEMBERS_SOURCE.into(), 1);
    (source, rows)
}
fn report(
    source: &CollectionDescription,
    rows: &[OperationalObservation],
    complete: bool,
) -> Result<GroupReplicationReport> {
    explain(GroupReplicationEvidence {
        source,
        observations: &rows.iter().collect::<Vec<_>>(),
        read_complete: complete,
    })
}
#[test]
fn observed_member_role_and_exact_queues_totals_are_visible_without_current_database_or_query_cause() {
    let (source, rows) = original();
    let report = report(&source, &rows, true).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.native_rows, Some(2));
    assert_eq!(report.observed_rows, 2);
    let entry = &report.entries[0];
    assert_eq!(entry.explanation, t!("view.group_replication.queued"));
    assert_eq!(entry.scope, MeasurementScope::Server);
    assert_eq!(entry.fields.iter().find(|f| f.key == "role").unwrap().value.as_deref(), Some("SECONDARY"));
    assert_eq!(entry.groups[0].fields[0].value.as_deref(), Some("0"));
    assert_eq!(entry.groups[0].fields[1].value.as_deref(), Some("3"));
    assert_eq!(entry.groups[0].fields[2].value, None);
    assert_eq!(
        entry.groups[1].fields[0].value.as_deref(),
        Some(format::digits("18446744073709551615".into()).as_str())
    );
    assert_eq!(entry.groups[1].fields[1].value.as_deref(), Some(format::digits("9007199254740993".into()).as_str()));
    assert_eq!(entry.investigation, t!("view.group_replication.investigate"));
    assert_eq!(entry.verify, t!("view.group_replication.verify"));
    assert!(report.limitations.contains(&t!("view.group_replication.limit.reset")));
    assert_eq!(report.original.unwrap().context.database_writable, None);
}
#[test]
fn native_error_unreachable_and_recovery_states_have_exact_meaning_without_changing_ordinary_zero_context() {
    for (state, key) in [
        ("ERROR", "view.group_replication.member_error"),
        ("UNREACHABLE", "view.group_replication.member_unreachable"),
        ("RECOVERING", "view.group_replication.member_recovering"),
        ("ONLINE", "view.group_replication.member_context"),
        ("unknown-future-state", "view.group_replication.member_context"),
    ] {
        let (source, mut rows) = original();
        let OperationalFact::MysqlGroupReplication(point) = &mut rows[0].fact else { panic!() };
        let MysqlGroupReplicationPoint::Member(member) = point.as_mut() else { panic!() };
        member.state = state.into();
        let OperationalFact::MysqlGroupReplication(point) = &mut rows[1].fact else { panic!() };
        let MysqlGroupReplicationPoint::Statistics(stats) = point.as_mut() else { panic!() };
        stats.certification_queue = Some("0".into());
        stats.applier_queue = Some("0".into());
        let report = report(&source, &rows, true).unwrap();
        assert_eq!(report.entries[0].explanation, culprit_core::i18n::t(key, &[]));
        assert_eq!(report.entries[0].groups[0].fields[1].value.as_deref(), Some("0"));
        assert!(report.entries[0].conditions.is_empty());
    }
}
#[test]
fn source_denial_and_missing_body_do_not_lend_other_members_roles_or_create_zero_metrics() {
    for denied in [MYSQL_GROUP_MEMBERS_SOURCE, MYSQL_GROUP_STATISTICS_SOURCE] {
        let (mut source, mut rows) = original();
        rows.retain(|row| row.source != denied);
        let SourceObservation::GroupReplication { snapshot, .. } = &mut source.observation else { panic!() };
        snapshot.records = 1;
        let item = snapshot.sources.iter_mut().find(|s| s.source == denied).unwrap();
        *item = StateSourceCoverage::unavailable(denied.into(), culprit_core::ErrorCode::PermissionDenied, None);
        let report = report(&source, &rows, true).unwrap();
        assert_eq!(report.sources.iter().find(|s| s.source == denied).unwrap().state, SourceState::Unavailable);
        let entry = &report.entries[0];
        if denied == MYSQL_GROUP_MEMBERS_SOURCE {
            assert_eq!(entry.title, t!("view.group_replication.statistics_title"));
            assert_eq!(entry.fields.iter().find(|f| f.key == "role").unwrap().value, None);
        } else {
            assert!(entry.groups.iter().flat_map(|g| &g.fields).all(|f| f.value.is_none()));
        }
    }
    let (source, rows) = original();
    let partial = report(&source, &rows[..1], false).unwrap();
    assert_eq!(partial.native_rows, Some(2));
    assert_eq!(partial.observed_rows, 1);
    assert!(partial.sources.iter().all(|s| s.state == SourceState::Partial
        && s.reason.as_deref() == Some(t!("view.group_replication.partial").as_str())));
    assert!(report(&source, &rows[..1], true).is_err());
}
#[test]
fn distinct_native_member_or_channel_never_borrows_a_counterpart_and_duplicate_or_foreign_rows_refuse() {
    let (source, mut rows) = original();
    let OperationalFact::MysqlGroupReplication(point) = &mut rows[1].fact else { panic!() };
    let MysqlGroupReplicationPoint::Statistics(stats) = point.as_mut() else { panic!() };
    stats.member_id = "other-member".into();
    rows[1].key = point.key().unwrap();
    let report = report(&source, &rows, true).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert!(report.entries.iter().any(|entry| entry.fields.iter().find(|f| f.key == "role").unwrap().value.is_none()));
    let (source, mut rows) = original();
    rows.push(rows[0].clone());
    assert!(
        super::explain(GroupReplicationEvidence {
            source: &source,
            observations: &rows.iter().collect::<Vec<_>>(),
            read_complete: false
        })
        .is_err()
    );
    let (source, mut rows) = original();
    let OperationalFact::MysqlGroupReplication(point) = &mut rows[1].fact else { panic!() };
    let MysqlGroupReplicationPoint::Statistics(stats) = point.as_mut() else { panic!() };
    stats.group_id = "foreign-group".into();
    rows[1].key = point.key().unwrap();
    assert!(self::report(&source, &rows, true).is_err());
}
#[test]
fn qualified_empty_and_unavailable_are_historical_context_and_withdrawal_loses_indivisible_native_authority() {
    let (mut source, _) = original();
    let SourceObservation::GroupReplication { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 0;
    for coverage in &mut snapshot.sources {
        *coverage = StateSourceCoverage::observed(coverage.source.clone(), 0, true);
    }
    let empty = report(&source, &[], true).unwrap();
    assert!(empty.entries.is_empty());
    assert!(empty.sources.iter().all(|s| s.state == SourceState::Empty));
    assert!(empty.limitations.contains(&t!("view.group_replication.limit.coverage")));
    let withdrawn = unavailable(&culprit_core::message!("view.group_replication.sharing_withheld"));
    assert!(withdrawn.original.is_none());
    assert_eq!(withdrawn.native_rows, None);
    assert!(withdrawn.entries.is_empty());
    assert!(withdrawn.sources.iter().all(|s| s.state == SourceState::Unavailable));
}
#[test]
fn bounded_member_presentation_preserves_original_native_counts_and_explicit_partiality() {
    let (mut source, rows) = original();
    let mut many = Vec::new();
    for n in 0..40 {
        let mut row = rows[0].clone();
        let OperationalFact::MysqlGroupReplication(point) = &mut row.fact else { panic!() };
        let MysqlGroupReplicationPoint::Member(member) = point.as_mut() else { panic!() };
        member.member_id = format!("member-{n}");
        row.key = point.key().unwrap();
        many.push(row);
    }
    let SourceObservation::GroupReplication { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.records = 40;
    snapshot.sources = vec![
        StateSourceCoverage::collected(MYSQL_GROUP_MEMBERS_SOURCE.into(), 40),
        StateSourceCoverage::observed(MYSQL_GROUP_STATISTICS_SOURCE.into(), 0, true),
    ];
    let result = report(&source, &many, true).unwrap();
    assert_eq!(result.entries.len(), MAX_GROUP_REPLICATION_ENTRIES);
    assert_eq!(result.native_rows, Some(40));
    assert_eq!(result.observed_rows, 40);
    assert!(result.entries_limited);
}
