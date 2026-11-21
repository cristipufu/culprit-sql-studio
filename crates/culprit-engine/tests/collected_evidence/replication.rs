use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{ReplicationCase, assert_replication_capture, replication_evidence, replication_evidence_cases},
};
use culprit_core::{ReplicationCondition, SourceState, telemetry::*};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ReplicationSamples];

fn snapshot(source: &CollectionDescription, records: &[TelemetryRecord]) -> ReplicationSnapshot {
    let rows = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Replication(row) = record else { panic!("native replication point") };
            row.clone()
        })
        .collect();
    ReplicationSnapshot::from_source(source, rows).unwrap()
}

#[tokio::test]
async fn all_native_replication_cases_reuse_the_pure_report_and_exact_verified_source_without_recovery_reads() {
    for (source, records) in replication_evidence_cases() {
        let expected = culprit_checks::replication::explain(&snapshot(&source, &records), true).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_replication_capture(&capture, &source, &records);
        let analysis = collected_resources::replication(&capture).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert_eq!(analysis.report.native_rows as usize, records.len());
        assert_eq!(analysis.report.observed_rows as usize, records.len());
        assert!(analysis.report.read_complete);
        for entry in &analysis.report.entries {
            assert!(records.iter().any(|record| matches!(record, TelemetryRecord::Replication(row)
                if row.source == entry.reference.source && row.fact.key() == entry.reference.observation_key)));
        }
        assert_eq!(capture.metrics().selected_rows as usize, records.len());
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_and_unpublished_replication_points_preserve_original_counts_and_cannot_claim_recovery() {
    let (source, records) = replication_evidence(ReplicationCase::PostgresSender);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::ReplicationSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::replication(&capture).unwrap();
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows as usize, records.len());
    assert_eq!(analysis.report.observed_rows as usize, records.len() - 1);
    assert_eq!(analysis.report.entries.len(), records.len() - 1);
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    for record in &analysis.context.records {
        assert!(records.contains(&record.row.record));
    }
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::replication(&capture).unwrap();
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.native_rows as usize, records.len());
    assert_eq!(analysis.report.observed_rows, 0);
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::NotPublished));
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn paused_vs_requested_and_synced_vs_invalid_slots_keep_independent_native_state_meanings() {
    for (case, paused) in
        [(ReplicationCase::PostgresStandbyPaused, true), (ReplicationCase::PostgresStandbyRequested, false)]
    {
        let (source, records) = replication_evidence(case);
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::replication(&capture).unwrap();
        let entry = &analysis.report.entries[0];
        assert_eq!(entry.conditions.contains(&ReplicationCondition::ReplayPaused), paused);
        assert!(entry.session.is_none());
        assert_eq!(entry.unresolved_session_id.as_deref(), Some("73"));
        let fields = entry.fields.iter().chain(entry.groups.iter().flat_map(|group| &group.fields)).collect::<Vec<_>>();
        assert_eq!(
            fields.iter().find(|field| field.key == "last_replay_commit_at").unwrap().value.as_deref(),
            Some("2026-11-15T12:00:00.444444Z")
        );
        assert!(fields.iter().all(|field| !field.key.contains("eta") && field.key != "lag_seconds"));
    }
    let (source, records) = replication_evidence(ReplicationCase::PostgresSlots);
    let fixture = Fixture::new(source, records.clone()).await;
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::replication(&capture).unwrap();
    assert_eq!(analysis.report.state_rows, 1);
    for record in &records {
        let TelemetryRecord::Replication(point) = record else { unreachable!() };
        let ReplicationFact::Native(sample) = &point.fact else { unreachable!() };
        let entry =
            analysis.report.entries.iter().find(|entry| entry.reference.observation_key == point.fact.key()).unwrap();
        if sample.slot_name.as_deref() == Some("synced_consumer") {
            assert!(entry.conditions.is_empty());
        } else {
            assert_eq!(entry.conditions, vec![ReplicationCondition::SlotInvalid]);
        }
    }
}

#[tokio::test]
async fn exact_positions_and_offset_free_native_clocks_do_not_gain_a_measured_epoch_or_timestamp_conversion() {
    for case in
        [ReplicationCase::MssqlAvailabilityGroup, ReplicationCase::MySqlChannel, ReplicationCase::MariaDbChannel]
    {
        let (source, records) = replication_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(FAMILY).await;
        assert_replication_capture(&capture, &source, &records);
        let analysis = collected_resources::replication(&capture).unwrap();
        let native = serde_json::to_value(&analysis.context.records).unwrap().to_string();
        let report = serde_json::to_value(&analysis.report).unwrap().to_string();
        match case {
            ReplicationCase::MssqlAvailabilityGroup => {
                assert!(native.contains("1234567890123456789012345"));
                assert!(native.contains("2026-11-16T01:02:03.123"));
                assert!(report.contains("1234567890123456789012345"));
                assert!(report.contains("2026-11-16T01:02:03.123"));
            }
            _ => {
                assert!(native.contains("9007199254740993"));
                assert!(report.contains("261116 01:02:03"));
                assert!(report.contains("2026-11-16 01:02:02"));
            }
        }
        assert!(analysis.report.context.native().unwrap().partition.native_epoch().is_none());
        assert!(analysis.report.entries.iter().all(|entry| entry.session.is_none()));
    }
}

#[tokio::test]
async fn unknown_local_owner_retains_positive_partial_points_without_authority_and_other_families_do_not_substitute() {
    let (source, records) = replication_evidence(ReplicationCase::UnqualifiedOwner);
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::replication(&capture).unwrap();
    assert!(analysis.report.read_complete);
    assert!(!analysis.report.context.native().unwrap().has_qualified_local_owner());
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert!(!analysis.report.entries.is_empty());
    assert_eq!(analysis.context.source, source);
    let wrong_family = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::replication(&wrong_family).is_err());
}

#[tokio::test]
async fn hyperscale_original_topology_stays_database_scoped_and_missing_archive_rows_never_gain_complete_tree_health() {
    for case in [
        ReplicationCase::MssqlHyperscale,
        ReplicationCase::MssqlHyperscaleNulls,
        ReplicationCase::MssqlHyperscalePartial,
        ReplicationCase::MssqlHyperscaleUnavailable,
    ] {
        let (source, records) = replication_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::replication(&capture).unwrap();
        assert_eq!(analysis.report.state_rows, 0);
        assert_eq!(analysis.report.context.native().unwrap().target, source.target);
        assert!(
            analysis
                .report
                .sources
                .iter()
                .all(|source| source.state != SourceState::Collected && source.state != SourceState::Empty)
        );
        for row in &records {
            let TelemetryRecord::Replication(row) = row else { panic!() };
            let ReplicationFact::HyperscaleReplica(point) = &row.fact else { panic!() };
            let entry =
                analysis.report.entries.iter().find(|entry| entry.reference.observation_key == row.fact.key()).unwrap();
            let field = |key: &str| entry.fields.iter().find(|field| field.key == key).unwrap().value.clone();
            assert_eq!(field("local_database_id"), point.local_database_id.map(|id| id.to_string()));
            assert_eq!(field("parent_replica_id"), point.parent_replica_id);
            assert_eq!(field("role_code"), point.replica_role.map(|role| role.to_string()));
            assert_eq!(entry.investigation, culprit_core::t!("view.replication.mssql.hyperscale.investigate"));
            assert_eq!(entry.verify, culprit_core::t!("view.replication.mssql.hyperscale.verify"));
            assert!(entry.conditions.is_empty() && entry.session.is_none());
        }
        if !records.is_empty() {
            fixture.remove_first_row(TelemetryStream::ReplicationSamples);
            let capture = fixture.read_families(FAMILY).await;
            let missing = collected_resources::replication(&capture).unwrap();
            assert!(!missing.report.read_complete);
            assert_eq!(missing.report.native_rows, 4);
            assert_eq!(missing.report.observed_rows, 3);
            assert_eq!(missing.context.source, source);
        }
    }
}
