use super::*;
use culprit_core::{CheckEvidence, CheckStatus, CleanupKind, operations::IndexUsageScope};

fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.taken_at = "2026-10-11T12:00:00Z".into();
    snapshot.server.started_at = Some("2026-10-01T12:00:00Z".into());
    snapshot.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    snapshot.foreign_keys.clear();
    let mut index = snapshot.indexes.iter().find(|index| !index.is_primary_key).unwrap().clone();
    index.name = "ix_scope_probe".into();
    index.columns = vec!["scope_probe".into()];
    index.include.clear();
    index.filter = None;
    index.is_primary_key = false;
    index.is_unique = false;
    index.is_clustered = false;
    index.is_disabled = false;
    index.partition_of = None;
    index.created_at_latest = None;
    index.reads = Some(0.0);
    index.writes = Some(10_000.0);
    index.size_bytes = Some(16.0 * 1024.0 * 1024.0);
    snapshot.indexes = vec![index];
    snapshot
}

#[test]
fn identical_zero_counts_and_a_week_require_explicit_all_read_member_coverage() {
    let mut snapshot = snapshot();
    assert!(watched_long_enough(&snapshot));
    for scope in [
        None,
        Some(IndexUsageScope::ConnectedMember),
        Some(IndexUsageScope::ObservedMembers),
        Some(IndexUsageScope::AllReadMembers),
    ] {
        snapshot.operations.index_usage_scope = scope;
        let qualified = scope == Some(IndexUsageScope::AllReadMembers);
        assert_eq!(is_unused(&snapshot, &snapshot.indexes[0]), qualified);
        assert_eq!(unused_index(&snapshot).len(), usize::from(qualified));
        let evidence = crate::evidence::assess(&[crate::evidence::Requirement::IndexHistory], &snapshot);
        assert_eq!(matches!(evidence, CheckEvidence::Available), qualified);
        if !qualified {
            assert!(matches!(evidence, CheckEvidence::Insufficient(_)));
        }
        assert_eq!(snapshot.indexes[0].reads, Some(0.0));
        assert_eq!(snapshot.indexes[0].writes, Some(10_000.0));
    }
    snapshot.indexes[0].reads = Some(17.0);
    assert!(!is_unused(&snapshot, &snapshot.indexes[0]), "positive usage never becomes absence");
    snapshot.indexes[0].reads = Some(0.0);
    snapshot.server.started_at = Some("2026-10-10T12:00:00Z".into());
    snapshot.pressure.as_mut().unwrap().uptime_hours = Some(24.0);
    assert!(!is_unused(&snapshot, &snapshot.indexes[0]), "global coverage does not invent a week of history");
}

#[test]
fn local_or_unknown_usage_is_unassessed_and_cannot_authorize_advisor_disable_sql() {
    for scope in [
        None,
        Some(IndexUsageScope::ConnectedMember),
        Some(IndexUsageScope::ObservedMembers),
        Some(IndexUsageScope::AllReadMembers),
    ] {
        let mut snapshot = snapshot();
        snapshot.operations.index_usage_scope = scope;
        let report = crate::analyze(snapshot);
        let qualified = scope == Some(IndexUsageScope::AllReadMembers);
        let assessment = report.assessments.iter().find(|assessment| assessment.check == "unused-index").unwrap();
        assert_eq!(
            assessment.status,
            if qualified { CheckStatus::Findings } else { CheckStatus::InsufficientEvidence }
        );
        let advice = crate::index_advice(&report);
        let unused = advice.cleanups.iter().filter(|cleanup| cleanup.kind == CleanupKind::Unused).collect::<Vec<_>>();
        assert_eq!(unused.len(), usize::from(qualified));
        if qualified {
            assert!(unused[0].sql.as_deref().is_some_and(|sql| sql.contains("DISABLE")));
        } else {
            assert!(advice.cleanups.is_empty());
            assert!(assessment.reason.is_some());
            assert!(report.summary.index_notes.iter().all(|note| note.finding.is_none()));
        }
        assert_eq!(report.snapshot.indexes[0].writes, Some(10_000.0));
    }
}

#[test]
fn local_zero_or_rare_usage_cannot_fall_through_to_overlapping_index_disable_advice() {
    for reads in [0.0, 5.0] {
        let mut snapshot = snapshot();
        snapshot.indexes[0].columns = vec!["scope_probe".into(), "region".into()];
        snapshot.indexes[0].reads = Some(reads);
        let mut wider = snapshot.indexes[0].clone();
        wider.name = "ix_scope_alternative".into();
        wider.columns = vec!["scope_probe".into(), "kind".into(), "region".into()];
        wider.reads = Some(100_000.0);
        snapshot.indexes.push(wider);
        snapshot.operations.index_usage_scope = Some(IndexUsageScope::AllReadMembers);
        let qualified = crate::index_advice(&crate::analyze(snapshot.clone()));
        assert!(qualified.cleanups.iter().any(|cleanup| {
            cleanup.index == "ix_scope_probe"
                && cleanup.sql.is_some()
                && cleanup.kind == if reads == 0.0 { CleanupKind::Unused } else { CleanupKind::Overlap }
        }));
        for scope in [None, Some(IndexUsageScope::ConnectedMember), Some(IndexUsageScope::ObservedMembers)] {
            snapshot.operations.index_usage_scope = scope;
            assert!(crate::index_health::overlapping_pairs(&snapshot).is_empty());
            let report = crate::analyze(snapshot.clone());
            assert!(crate::index_advice(&report).cleanups.is_empty());
            assert_eq!(report.snapshot.indexes[0].reads, Some(reads));
            let assessment =
                report.assessments.iter().find(|assessment| assessment.check == "overlapping-index").unwrap();
            assert_eq!(assessment.status, CheckStatus::InsufficientEvidence);
        }
    }
}

#[test]
fn incomplete_usage_does_not_remove_catalog_proven_duplicate_cleanup() {
    let mut snapshot = snapshot();
    snapshot.operations.index_usage_scope = Some(IndexUsageScope::ConnectedMember);
    let mut twin = snapshot.indexes[0].clone();
    twin.name = "ix_scope_twin".into();
    snapshot.indexes.push(twin);
    let advice = crate::index_advice(&crate::analyze(snapshot));
    assert_eq!(advice.cleanups.len(), 1);
    assert_eq!(advice.cleanups[0].kind, CleanupKind::Duplicate);
    assert!(advice.cleanups[0].sql.is_some());
}

#[test]
fn current_member_totals_with_old_epochs_do_not_authorize_global_cleanup_after_historical_membership_loss() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/yugabyte-demo.json")).unwrap();
    snapshot.taken_at = "2026-10-11T12:00:00Z".into();
    snapshot.server.started_at = Some("2020-01-01T00:00:00Z".into());
    let mut index = snapshot.indexes.iter().find(|index| !index.is_primary_key).unwrap().clone();
    index.reads = Some(0.0);
    index.size_bytes = Some(16.0 * 1024.0 * 1024.0);
    index.counters_since = Some("2026-10-01T00:00:00Z".into());
    index.created_at_latest = None;
    snapshot.indexes = vec![index];
    snapshot.foreign_keys.clear();
    snapshot.operations.index_usage_scope = Some(IndexUsageScope::ObservedMembers);
    let usage =
        snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::DOCDB_INDEX_USAGE).unwrap();
    usage.state = culprit_core::SourceState::Collected;
    usage.reason = None;
    usage.reason_message = None;
    snapshot.server.missing.retain(|missing| missing.capability != culprit_core::Capability::IndexUsage);
    snapshot.server.capabilities.push(culprit_core::Capability::IndexUsage);

    assert!(watched_long_enough(&snapshot), "the current contributors' old epochs establish a week of counters");
    assert!(!is_unused(&snapshot, &snapshot.indexes[0]), "a departed member can hold unobserved reads");
    let report = crate::analyze(snapshot);
    let assessment = report.assessments.iter().find(|assessment| assessment.check == "unused-index").unwrap();
    assert_eq!(assessment.status, CheckStatus::InsufficientEvidence);
    assert!(assessment.reason.is_some());
    assert!(!report.findings.iter().any(|finding| finding.check == "unused-index"));
    assert!(crate::index_advice(&report).cleanups.is_empty());
    assert_eq!(report.snapshot.indexes[0].reads, Some(0.0));
}
