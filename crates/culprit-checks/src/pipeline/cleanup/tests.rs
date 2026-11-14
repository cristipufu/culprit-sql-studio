use super::*;
use crate::waits::snapshot_with;
use culprit_core::{
    CleanupHolder, CleanupMechanism, Engine, MeasurementScope, TransactionActivity, source_id, telemetry::*,
};
use serde_json::json;

fn snapshot() -> Snapshot {
    let mut snapshot = snapshot_with(Engine::Mysql);
    snapshot.sources.clear();
    snapshot.pressure = None;
    snapshot.operations.cleanup.clear();
    snapshot
}

fn outcome(source: &str, state: SourceState) -> SourceOutcome {
    SourceOutcome { source: source.into(), state, reason: None, reason_message: None }
}

fn native_read(source: &str, fact: OperationalFact) -> CleanupSnapshot {
    CleanupSnapshot {
        source: source.into(),
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        observations: vec![OperationalObservation { source: source.into(), key: "original-native-point".into(), fact }],
    }
}

fn persistent() -> CleanupSnapshot {
    let store: MssqlPersistentVersionStore = serde_json::from_value(json!({
        "available_columns": ["persistent_version_store_size_kb", "pvs_off_row_page_skipped_oldest_active_xdesid"],
        "off_row_kibibytes": "9007199254740993",
        "pages_skipped_for_oldest_active_transaction": "0",
        "off_row_cleanup": {},
        "aborted_cleanup": {}
    }))
    .unwrap();
    native_read(source_id::PERSISTENT_VERSION_STORE, OperationalFact::MssqlPersistentVersionStore(Box::new(store)))
}

fn transaction() -> MssqlCleanupTransaction {
    MssqlCleanupTransaction {
        session: NativeActivityIdentity { id: "42".into(), generation: "native-session-generation".into() },
        transaction: NativeActivityIdentity {
            id: "9007199254740993".into(),
            generation: "native-transaction-generation".into(),
        },
        observed_time_without_offset: "2020-01-01T08:00:00.1533333".into(),
        age_seconds: Some("60".into()),
        program: None,
        native_status: Some("sleeping".into()),
        log_record_count: Some("1".into()),
        log_bytes_used: Some("0".into()),
        begin_lsn: None,
        row_version_participant: false,
        snapshot_isolation: None,
        transaction_sequence_number: None,
        versioning_age_seconds: None,
    }
}

fn horizon(empty: bool) -> CleanupSnapshot {
    let mut read = native_read(
        source_id::OPEN_TRANSACTIONS,
        OperationalFact::MssqlCleanupHorizon(MssqlCleanupHorizon {
            oldest_confirmed: (!empty).then(|| Box::new(transaction())),
            oldest_unattributed_versioning: None,
        }),
    );
    read.observed_from = "2026-11-13T10:05:00Z".into();
    read.observed_until = "2026-11-13T10:05:01Z".into();
    read
}

fn holder(snapshot: &Snapshot, id: &str) -> RetentionObservation {
    RetentionObservation {
        source: "embedded.retainer-source".into(),
        identity: NativeIdentity::new(id),
        observed_at: snapshot.taken_at.clone(),
        scope: MeasurementScope::Database,
        holder: CleanupHolder::Session,
        name: id.into(),
        horizons: vec![RetentionHorizon {
            protects: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
            native_name: "xmin".into(),
            native_value: "1234".into(),
            transactions_behind: Some("9007199254740993".into()),
        }],
        session: Some(NativeActivityIdentity { id: id.into(), generation: "native-backend-generation".into() }),
        unresolved_session_id: None,
        transaction: None,
        query: Some(RetentionQueryContext {
            identity: None,
            started_at: Some(snapshot.taken_at.clone()),
            relationship: RetentionQueryRelationship::PreviousStatement,
        }),
        client: Some(ActivityClient { application: Some("native-app-canary".into()), user: None, host: None }),
        activity: TransactionActivity::IdleInTransaction,
        native_state: Some("native-idle-state".into()),
        age_seconds: None,
        database: Some(snapshot.server.database.clone()),
        code_ownership: PayloadUnavailable::NotCollected,
    }
}

fn install_retention(snapshot: &mut Snapshot, holders: Vec<RetentionObservation>) {
    let source = holders[0].source.clone();
    snapshot.pressure.get_or_insert_default().retention = Some(RetentionSnapshot {
        sources: vec![RetentionSourceCoverage { source: source.clone(), state: SourceState::Collected }],
        coverage: RetentionCoverage {
            native_sources: vec![source.clone()],
            qualified_horizons: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
        },
        holders,
    });
    snapshot.sources.push(outcome(&source, SourceState::Collected));
}

#[test]
fn scan_pipeline_reuses_the_pure_view_for_independent_native_points_and_tied_retention_horizons() {
    let mut snapshot = snapshot();
    snapshot.operations.cleanup = vec![persistent(), horizon(false)];
    snapshot.sources = vec![
        outcome(source_id::PERSISTENT_VERSION_STORE, SourceState::Collected),
        outcome(source_id::OPEN_TRANSACTIONS, SourceState::Collected),
    ];
    let holders = vec![holder(&snapshot, "42"), holder(&snapshot, "43")];
    install_retention(&mut snapshot, holders);
    let unrelated = outcome("unrelated-native-cpu", SourceState::Unavailable);
    let expected = cleanup::explain(
        CleanupEvidence {
            retention: snapshot.pressure.as_ref().unwrap().retention.as_ref(),
            native: &snapshot.operations.cleanup.iter().flat_map(|read| &read.observations).collect::<Vec<_>>(),
        },
        &snapshot.sources,
    )
    .unwrap();
    snapshot.sources.push(unrelated.clone());
    let raw = snapshot.operations.cleanup.clone();
    let retained = snapshot.pressure.as_ref().unwrap().retention.clone();
    let report = crate::analyze(snapshot);
    let explained = report.cleanup.as_ref().unwrap();
    assert_eq!(serde_json::to_value(explained).unwrap(), serde_json::to_value(expected).unwrap());
    assert!(!explained.sources.iter().any(|source| source.source == unrelated.source));
    assert_eq!(report.snapshot.operations.cleanup, raw);
    assert_eq!(report.snapshot.pressure.as_ref().unwrap().retention, retained);
    assert_eq!(explained.entries.len(), 4);
    let data = explained.boundaries.iter().find(|boundary| boundary.horizon == CleanupHorizon::Data).unwrap();
    assert!(data.complete);
    assert_eq!(data.oldest_observed.len(), 2);
    assert_ne!(raw[0].observed_from, raw[1].observed_from, "the pipeline never merges separate acquisition windows");
}

#[test]
fn semantic_empty_native_horizon_and_failed_canonical_sources_keep_their_original_coverage() {
    for state in [SourceState::Empty, SourceState::Collected] {
        let mut snapshot = snapshot();
        snapshot.operations.cleanup = vec![horizon(true)];
        let mut unavailable = outcome(source_id::PERSISTENT_VERSION_STORE, SourceState::Unavailable);
        unavailable.reason = Some("original native permission failure".into());
        snapshot.sources = vec![outcome(source_id::OPEN_TRANSACTIONS, state), unavailable.clone()];
        let original = snapshot.sources.clone();
        let report = crate::analyze(snapshot);
        let explained = report.cleanup.as_ref().unwrap();
        assert_eq!(explained.sources, original);
        assert_eq!(report.snapshot.sources, original);
        assert_eq!(explained.entries.len(), 1);
        assert!(explained.entries[0].mechanisms.is_empty());
        assert!(explained.boundaries.iter().all(|boundary| !boundary.complete));
        assert!(!report.snapshot.notes.iter().any(|note| note.message == culprit_core::t!("view.cleanup.invalid")));
    }
}

#[test]
fn invalid_native_wrappers_withhold_only_their_explanation_and_preserve_raw_facts() {
    for defect in ["clock", "duplicate-key", "foreign-source", "column-unavailable"] {
        let mut snapshot = snapshot();
        let mut invalid = persistent();
        match defect {
            "clock" => invalid.observed_until = "2026-11-13T09:59:59Z".into(),
            "duplicate-key" => invalid.observations.push(invalid.observations[0].clone()),
            "foreign-source" => invalid.observations[0].source = "different-native-source".into(),
            "column-unavailable" => {
                let OperationalFact::MssqlPersistentVersionStore(store) = &mut invalid.observations[0].fact else {
                    unreachable!()
                };
                store.available_columns.retain(|column| column != "persistent_version_store_size_kb");
            }
            _ => unreachable!(),
        }
        snapshot.operations.cleanup = vec![invalid, horizon(false)];
        snapshot.sources = vec![
            outcome(source_id::PERSISTENT_VERSION_STORE, SourceState::Collected),
            outcome(source_id::OPEN_TRANSACTIONS, SourceState::Collected),
        ];
        let raw = snapshot.operations.cleanup.clone();
        let report = crate::analyze(snapshot);
        let explained = report.cleanup.as_ref().unwrap();
        assert_eq!(explained.entries.len(), 1, "{defect}");
        assert_eq!(explained.entries[0].mechanisms, [CleanupMechanism::ConfirmedDatabaseTransaction]);
        assert_eq!(report.snapshot.operations.cleanup, raw);
        assert_eq!(report.evidence.as_ref().unwrap().input.operations.cleanup, raw);
        assert!(report.snapshot.notes.iter().any(|note| note.collector == source_id::PERSISTENT_VERSION_STORE
            && note.message_source.as_ref() == Some(&culprit_core::message!("view.cleanup.invalid"))));
        assert!(
            explained.sources.iter().any(|source| source.source == source_id::PERSISTENT_VERSION_STORE
                && source.state == SourceState::Unavailable)
        );
    }
}

#[test]
fn unknown_retention_qualification_preserves_native_holders_without_claiming_a_complete_boundary() {
    let mut snapshot = snapshot();
    let original = holder(&snapshot, "42");
    install_retention(&mut snapshot, vec![original.clone()]);
    snapshot.pressure.as_mut().unwrap().retention.as_mut().unwrap().coverage.native_sources.clear();
    let raw = snapshot.pressure.as_ref().unwrap().retention.clone();
    let report = crate::analyze(snapshot);
    let explained = report.cleanup.as_ref().unwrap();
    assert_eq!(explained.entries.len(), 1);
    assert_eq!(explained.entries[0].reference.identity.as_ref(), Some(&original.identity));
    assert_eq!(explained.entries[0].mechanisms, [CleanupMechanism::SessionHorizon]);
    assert!(explained.boundaries.iter().all(|boundary| !boundary.complete));
    assert!(explained.limitations.contains(&culprit_core::t!("view.cleanup.invalid")));
    assert_eq!(report.snapshot.pressure.as_ref().unwrap().retention, raw);
}

#[test]
fn invalid_retention_rows_do_not_erase_other_positive_holders_or_independent_pvs_facts() {
    for (scope, database) in [
        (MeasurementScope::Database, Some("another-database")),
        (MeasurementScope::Database, None),
        (MeasurementScope::Node, Some("selected")),
        (MeasurementScope::Pool, Some("selected")),
        (MeasurementScope::Session, Some("selected")),
        (MeasurementScope::Cluster, Some("selected")),
        (MeasurementScope::File, Some("selected")),
        (MeasurementScope::Unknown, Some("selected")),
    ] {
        let mut snapshot = snapshot();
        let good = holder(&snapshot, "42");
        let mut bad = holder(&snapshot, "43");
        bad.scope = scope;
        bad.database =
            database.map(|name| if name == "selected" { snapshot.server.database.clone() } else { name.into() });
        install_retention(&mut snapshot, vec![good.clone(), bad]);
        snapshot.operations.cleanup = vec![persistent()];
        snapshot.sources.push(outcome(source_id::PERSISTENT_VERSION_STORE, SourceState::Collected));
        let original = snapshot.pressure.as_ref().unwrap().retention.clone();
        let report = crate::analyze(snapshot);
        let explained = report.cleanup.as_ref().unwrap();
        assert_eq!(explained.entries.len(), 2);
        assert!(explained.entries.iter().any(|entry| entry.reference.identity.as_ref() == Some(&good.identity)));
        assert!(
            explained.entries.iter().any(|entry| entry.mechanisms.contains(&CleanupMechanism::PersistentVersionStore))
        );
        assert!(explained.boundaries.iter().all(|boundary| !boundary.complete));
        assert_eq!(report.snapshot.pressure.as_ref().unwrap().retention, original);
        assert!(
            explained.sources.iter().any(|source| source.source == good.source && source.state == SourceState::Partial)
        );
    }
}

#[test]
fn server_scoped_slot_or_walsender_context_can_name_another_database_without_becoming_its_exclusive_owner() {
    for kind in [CleanupHolder::ReplicationSlot, CleanupHolder::Session] {
        let mut snapshot = snapshot();
        let mut global = holder(&snapshot, "server-holder");
        global.scope = MeasurementScope::Server;
        global.database = Some("another-database".into());
        global.holder = kind;
        install_retention(&mut snapshot, vec![global.clone()]);
        let report = crate::analyze(snapshot);
        let explained = report.cleanup.unwrap();
        assert_eq!(explained.entries.len(), 1);
        assert_eq!(explained.entries[0].reference.identity.as_ref(), Some(&global.identity));
        assert!(
            explained.boundaries.iter().any(|boundary| boundary.horizon == CleanupHorizon::Data && boundary.complete)
        );
        assert_eq!(report.snapshot.pressure.unwrap().retention.unwrap().holders, vec![global]);
    }
}

#[test]
fn contradictory_unavailable_native_rows_keep_the_original_failure_and_other_positive_sources() {
    let mut snapshot = snapshot();
    snapshot.operations.cleanup = vec![persistent(), horizon(false)];
    let mut unavailable = outcome(source_id::PERSISTENT_VERSION_STORE, SourceState::Unavailable);
    unavailable.reason = Some("original inaccessible source".into());
    snapshot.sources = vec![unavailable.clone(), outcome(source_id::OPEN_TRANSACTIONS, SourceState::Collected)];
    let report = crate::analyze(snapshot);
    let explained = report.cleanup.as_ref().unwrap();
    assert_eq!(explained.entries.len(), 1);
    assert_eq!(explained.entries[0].mechanisms, [CleanupMechanism::ConfirmedDatabaseTransaction]);
    assert_eq!(explained.sources.iter().find(|source| source.source == unavailable.source), Some(&unavailable));
    assert_eq!(report.snapshot.sources[0], unavailable);
}

#[test]
fn explicit_zero_row_custom_source_keeps_its_failure_but_unrelated_sources_do_not_create_a_cleanup_report() {
    let mut snapshot = snapshot();
    let mut source = outcome("embedded.cleanup-source", SourceState::Unavailable);
    source.reason = Some("native source not available".into());
    snapshot.operations.cleanup = vec![CleanupSnapshot {
        source: source.source.clone(),
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        observations: vec![],
    }];
    snapshot.sources = vec![source.clone(), outcome("unrelated-native-cpu", SourceState::Unavailable)];
    let report = crate::analyze(snapshot.clone());
    assert_eq!(report.cleanup.unwrap().sources, vec![source]);
    snapshot.operations.cleanup.clear();
    snapshot.sources.remove(0);
    assert!(crate::analyze(snapshot).cleanup.is_none());
}

#[test]
fn source_outcomes_and_legacy_display_values_alone_never_become_a_typed_empty_cleanup_population() {
    for state in [SourceState::Collected, SourceState::Empty, SourceState::Partial, SourceState::Unavailable] {
        let mut snapshot = snapshot();
        snapshot.sources = vec![outcome(source_id::OPEN_TRANSACTIONS, state), outcome(source_id::CLEANUP_SLOTS, state)];
        snapshot.pressure.get_or_insert_default().oldest_transaction = Some(culprit_core::OpenTransaction {
            session: "legacy-display-only".into(),
            seconds: 3600.0,
            ..Default::default()
        });
        assert!(crate::analyze(snapshot).cleanup.is_none());
    }
}
