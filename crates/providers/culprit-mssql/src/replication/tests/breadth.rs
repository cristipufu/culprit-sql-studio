use super::*;
use culprit_core::telemetry::TelemetryRecord;

#[tokio::test]
async fn database_group_and_replica_scopes_retain_exact_native_positions_clocks_and_connection_metadata() {
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![availability()])]);
    let rows = observed(read(&mut reader, 3, Selection::AvailabilityGroup, 100).await.unwrap());
    let ReplicationFact::AvailabilityGroup(row) = &rows[0] else { panic!() };
    assert_eq!(row.database_id, Some(5));
    assert_eq!(row.database_name.as_deref(), Some("shop"));
    assert_eq!(row.progress.recovery_lsn.as_deref(), Some("9999999999999999999999999"));
    assert_eq!(row.progress.last_sent_lsn.as_deref(), Some("9007199254740993"));
    assert_eq!(row.progress.last_hardened_lsn.as_deref(), Some("0"));
    assert!(row.progress.truncation_lsn.is_none() && row.progress.last_received_without_offset.is_none());
    assert_eq!(row.progress.last_sent_without_offset.as_deref(), Some("2020-01-01T01:00:00.0030000"));
    assert_eq!(row.progress.filestream_send_rate_kb_per_second.as_deref(), Some("0"));
    assert_eq!(row.progress.is_commit_participant, Some(false));
    assert_eq!(row.progress.database_state, Some(0));
    let group = row.group.as_ref().unwrap();
    assert!(group.catalog_observed && group.state_observed);
    assert_eq!(group.synchronization_health, Some(1), "group rollup is not this database's synchronization health");
    assert_eq!(row.progress.synchronization_health, Some(0));
    assert!(group.secondary_recovery_health.is_none(), "primary-side unknowns remain unknown");
    assert_eq!(group.configuration_sequence.as_deref(), Some("9007199254740993"));
    let replica = row.replica.as_ref().unwrap();
    assert_eq!(replica.connected_state, Some(0));
    assert_eq!(replica.operational_state, None, "remote operational state is not borrowed from the local primary");
    assert_eq!(replica.last_connect_error_number, Some(35206));
    assert_eq!(replica.last_connect_error_without_offset.as_deref(), Some("2020-01-01T00:59:00.0030000"));
    assert_eq!(replica.secondary_role_allow_connections, Some(1));
    assert!(replica.endpoint_url.as_ref().unwrap().contains("native-replica.invalid"));
    assert_eq!(reader.calls.len(), 2, "existing layout plus one shared native fact query");
    let sql = &reader.calls[1].sql;
    assert!(sql.contains("WHERE drs.database_id=DB_ID()"));
    assert!(sql.contains("ars.replica_id=drs.replica_id AND ars.group_id=drs.group_id"));
    assert!(sql.contains("ags.group_id=drs.group_id"));
    assert!(sql.contains("CONVERT(varchar(25),drs.recovery_lsn)"));
    assert!(!sql.contains("last_connect_error_description"));
    assert!(!sql.contains("quorum_commit") && !sql.contains("write_lease_remaining_ticks"));
    let serialized = serde_json::to_vec(&rows).unwrap();
    assert_eq!(serde_json::from_slice::<Vec<ReplicationFact>>(&serialized).unwrap(), rows);
}

#[tokio::test]
async fn optional_versioned_columns_are_decided_before_the_native_read_without_schema_retry() {
    let absent = replace(
        &layout(Val::Int(1), 1),
        &[
            ("distributed_column", Val::Int(0)),
            ("basic_column", Val::Int(0)),
            ("dtc_column", Val::Int(0)),
            ("database_failover_column", Val::Int(0)),
            ("cluster_column", Val::Int(0)),
            ("required_secondaries_column", Val::Int(0)),
            ("sequence_column", Val::Int(0)),
            ("seeding_column", Val::Int(0)),
            ("read_write_routing_column", Val::Int(0)),
        ],
    );
    let row = replace(
        &availability(),
        &[
            ("group_is_distributed", Val::Null),
            ("group_is_basic", Val::Null),
            ("group_dtc_support", Val::Null),
            ("group_database_failover", Val::Null),
            ("group_cluster_type", Val::Null),
            ("group_native_cluster_type", Val::Null),
            ("group_required_secondaries", Val::Null),
            ("group_configuration_sequence", Val::Null),
            ("replica_seeding_mode", Val::Null),
            ("replica_native_seeding_mode", Val::Null),
            ("replica_read_write_routing_url", Val::Null),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![absent]), Ok(vec![row])]);
    let rows = observed(read(&mut reader, 3, Selection::AvailabilityGroup, 100).await.unwrap());
    assert_eq!(reader.calls.len(), 2);
    let sql = &reader.calls[1].sql;
    for unsupported in [
        "ag.is_distributed",
        "ag.basic_features",
        "ag.dtc_support",
        "ag.db_failover",
        "ag.cluster_type",
        "ag.required_synchronized_secondaries_to_commit",
        "ag.sequence_number",
        "ar.seeding_mode",
        "ar.read_write_routing_url",
    ] {
        assert!(!sql.contains(unsupported), "unsupported column {unsupported} must not be bound by SQL Server");
    }
    let ReplicationFact::AvailabilityGroup(row) = &rows[0] else { panic!() };
    assert!(row.group.as_ref().unwrap().is_distributed.is_none());
    assert!(row.replica.as_ref().unwrap().seeding_mode.is_none());
    assert!(row.progress.last_sent_lsn.is_some(), "known older-layout fields remain available");
}

fn absent_context(row: &Rec, prefix: &str, except: &[&str]) -> Rec {
    Rec::of(
        &row.columns()
            .map(|(name, value)| {
                (name, if name.starts_with(prefix) && !except.contains(&name) { Val::Null } else { value.clone() })
            })
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn hidden_group_and_replica_contexts_preserve_database_facts_without_complete_inventory_or_healthy_defaults() {
    let row = absent_context(&primary(), "group_", &["group_id", "group_database_id"]);
    let row = absent_context(&row, "replica_", &["replica_id"]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![row])]);
    let (records, sources) = read_selected(&mut reader, 3, 100).await.unwrap().into_records(true, None);
    assert_eq!(sources[0].state, culprit_core::SourceState::Partial);
    assert!(!sources[0].complete_scope);
    let TelemetryRecord::Replication(row) = &records[0] else { panic!() };
    let ReplicationFact::AvailabilityGroup(row) = &row.fact else { panic!() };
    assert!(row.group.is_none() && row.replica.is_none());
    assert_eq!(row.progress.last_commit_lsn.as_deref(), Some("41"));
    assert_eq!(reader.calls.len(), 2);
}

#[tokio::test]
async fn malformed_native_positions_clocks_or_context_identity_stop_before_another_replication_family() {
    for (field, value) in [
        ("recovery_lsn", Val::Text("10000000000000000000000000".into())),
        ("last_sent_lsn", Val::Text("1e20".into())),
        ("last_hardened_lsn", Val::Text("00".into())),
        ("last_redone_lsn", Val::Int(42)),
        ("last_sent_without_offset", Val::Text("2020-01-01T01:00:00Z".into())),
        ("last_commit_without_offset", Val::Text("2020-01-01T01:00:00.000000001".into())),
        ("group_catalog_id", Val::Text("22222222-2222-3333-4444-555555555555".into())),
        ("replica_state_id", Val::Text("22222222-2222-3333-4444-555555555555".into())),
        ("replica_last_connect_error_number", Val::Int(i64::MAX)),
    ] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout(Val::Null, 1)]),
            Ok(vec![replace(&availability(), &[(field, value)])]),
            Ok(vec![geo()]),
        ]);
        assert!(read_selected(&mut reader, 8, 100).await.is_err(), "{field}");
        assert_eq!(reader.calls.len(), 2, "invalid native field cannot authorize Geo follow-up: {field}");
    }
}

#[tokio::test]
async fn geo_commit_and_read_access_are_available_on_secondary_without_primary_lag_or_clock_borrowing() {
    let row = replace(
        &geo(),
        &[
            ("role", Val::Int(1)),
            ("native_role", Val::Text("SECONDARY".into())),
            ("lag_seconds", Val::Null),
            ("last_replication_at", Val::Null),
            ("secondary_allow_connections", Val::Int(0)),
            ("native_secondary_allow_connections", Val::Text("No".into())),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row])]);
    let rows = observed(read(&mut reader, 5, Selection::GeoLinks, 100).await.unwrap());
    let ReplicationFact::GeoLink(row) = &rows[0] else { panic!() };
    assert_eq!(row.last_commit_at.as_deref(), Some("2020-01-01T00:00:00.123456700Z"));
    assert_eq!(row.secondary_allow_connections, Some(0));
    assert!(row.lag_seconds.is_none() && row.last_replication_at.is_none());
    assert_eq!(reader.calls.len(), 1);
}

fn scan_snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.pressure = Some(Pressure::default());
    snapshot.settings.clear();
    snapshot.sources.clear();
    snapshot.notes.clear();
    snapshot
}

#[tokio::test]
async fn scan_retains_the_same_raw_read_with_actual_family_visibility_but_no_borrowed_native_owner() {
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![primary(), availability()])]);
    let mut snapshot = scan_snapshot();
    collect_from(&mut reader, 3, "controlled permission guidance", &mut snapshot).await;
    let pressure = snapshot.pressure.as_ref().unwrap();
    let raw = pressure.replication.as_ref().unwrap();
    assert!(raw.is_valid());
    assert!(matches!(raw.context, culprit_core::telemetry::ObservationReadContext::Collector { .. }));
    assert!(raw.context.native().is_none());
    assert_eq!(raw.sources[0].state, culprit_core::SourceState::Collected);
    assert!(raw.sources[0].complete_scope, "native query visibility is independent of process identity authority");
    assert_eq!(raw.observations.len(), 2);
    let ReplicationFact::AvailabilityGroup(remote) = &raw.observations[1].fact else { panic!() };
    assert_eq!(remote.progress.recovery_lsn.as_deref(), Some("9999999999999999999999999"));
    assert_eq!(pressure.replicas.len(), 1);
    assert_eq!(reader.calls.len(), 2);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn unconfirmed_scan_shutdown_publishes_neither_native_facts_nor_legacy_metrics_or_secondary_role() {
    let secondary = replace(&availability(), &[("is_local", Val::Int(1))]);
    for fail_read in [false, true] {
        let reply =
            if fail_read { Err(ReadError::Stopped(CollectionStop::TimedOut)) } else { Ok(vec![secondary.clone()]) };
        let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), reply]);
        reader.shutdown = culprit_core::telemetry::SourceShutdown::Unconfirmed;
        let mut snapshot = scan_snapshot();
        collect_from(&mut reader, 3, "controlled permission guidance", &mut snapshot).await;
        let pressure = snapshot.pressure.as_ref().unwrap();
        assert!(pressure.replication.is_none() && pressure.replicas.is_empty());
        assert!(snapshot.settings.iter().all(|setting| setting.name != culprit_core::REPLICA_SETTING));
        assert!(snapshot.sources.iter().any(|source| source.source == culprit_core::source_id::REPLICATION
            && source.state == culprit_core::SourceState::Unavailable));
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn confirmed_scan_preserves_a_failed_native_family_and_the_other_family_without_more_reads() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout(Val::Null, 1)]),
        Ok(vec![primary(), availability()]),
        Ok(vec![unavailable(229)]),
    ]);
    let mut snapshot = scan_snapshot();
    collect_from(&mut reader, 8, "controlled permission guidance", &mut snapshot).await;
    let raw = snapshot.pressure.as_ref().unwrap().replication.as_ref().unwrap();
    assert!(raw.is_valid());
    assert_eq!(raw.sources[0].state, culprit_core::SourceState::Collected);
    assert_eq!(raw.sources[1].state, culprit_core::SourceState::Unavailable);
    assert_eq!(raw.sources[1].error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(!raw.sources[1].complete_scope);
    assert_eq!(raw.observations.len(), 2);
    assert_eq!(snapshot.pressure.as_ref().unwrap().replicas.len(), 1);
    assert_eq!(reader.calls.len(), 3);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn unconfirmed_shutdown_keeps_the_existing_native_family_failure_without_publishing_other_family_facts() {
    let secondary = replace(&availability(), &[("is_local", Val::Int(1))]);
    let mut reader =
        FakeRead::new(vec![Ok(vec![layout(Val::Null, 1)]), Ok(vec![secondary]), Ok(vec![unavailable(229)])]);
    reader.shutdown = culprit_core::telemetry::SourceShutdown::Unconfirmed;
    let mut snapshot = scan_snapshot();
    collect_from(&mut reader, 8, "controlled permission guidance", &mut snapshot).await;
    assert!(snapshot.pressure.as_ref().unwrap().replication.is_none());
    assert!(snapshot.pressure.as_ref().unwrap().replicas.is_empty());
    assert!(snapshot.settings.iter().all(|setting| setting.name != culprit_core::REPLICA_SETTING));
    for source in [Selection::GeoLinks.source(), culprit_core::source_id::REPLICATION] {
        assert!(
            snapshot
                .sources
                .iter()
                .any(|outcome| outcome.source == source && outcome.state == culprit_core::SourceState::Unavailable)
        );
    }
    assert!(snapshot.notes.iter().any(|note| note.collector == Selection::GeoLinks.source()
        && note.message == culprit_core::t!("provider.mssql.replication.native_unavailable")));
    assert!(snapshot.notes.iter().any(|note| note.collector == culprit_core::source_id::REPLICATION
        && note.message == CollectionStop::CancellationUnconfirmed.error().message));
    assert_eq!(reader.calls.len(), 3);
    assert_eq!(reader.closed, 1);
}
