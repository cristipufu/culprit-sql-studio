use super::*;
use crate::statement_grant_fixture;
use culprit_core::{
    RuntimePopulation, RuntimePopulationCatalog, SnapshotOptions,
    i18n::Message,
    telemetry::{NativeIdentity, SourceObservation, SourcePartition},
};

fn snapshot() -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.options = Some(SnapshotOptions::default());
    statement_grant_fixture::install(&mut snapshot);
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.query_hash = snapshot.pressure.as_ref().unwrap().grants[0].observations[0].context.query_hash.clone();
    query.waits = vec![culprit_core::WaitStat {
        mechanism: None,
        category: "Memory".into(),
        wait_type: None,
        wait_ms: query.total_time_ms * 0.5,
        tasks: None,
    }];
    snapshot
}

fn scoped(snapshot: &mut Snapshot, group: Option<(&str, bool)>) {
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = group.map(|(id, _)| NativeIdentity::new(id));
    workload.population_catalog = Some(RuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: source_id::QUERY_STORE.into(),
        partitioned: group.is_some(),
        requires_selection: group.is_some_and(|(_, automatic)| !automatic),
        populations: group
            .map(|(id, _)| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from("native group"),
                native_properties: Default::default(),
            })
            .into_iter()
            .collect(),
    });
    snapshot.options.as_mut().unwrap().workload_population =
        group.filter(|(_, automatic)| !automatic).map(|(id, _)| NativeIdentity::new(id));
}

#[test]
fn matching_hashes_cannot_borrow_unpartitioned_selected_or_automatic_workload_evidence() {
    let original = snapshot();
    let expected = memory_grant(&original).remove(0);
    for group in [None, Some(("1", false)), Some(("7", false)), Some(("7", true))] {
        let mut snapshot = original.clone();
        scoped(&mut snapshot, group);
        let findings = memory_grant(&snapshot);
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.id, expected.id);
        assert_eq!(finding.actionability, Actionability::Information);
        assert_eq!(finding.severity, Severity::Low);
        assert!(finding.queries.is_empty());
        assert_eq!(finding.impact.db_time_share, None);
        assert_eq!(finding.impact.time_ms_per_day, None);
        assert!(finding.fix.as_ref().unwrap().sql.is_none());
        let reference = finding.facts.statement_grant.as_ref().unwrap();
        let read = &snapshot.pressure.as_ref().unwrap().grants[0];
        assert_eq!(reference.snapshot_key, read.key().unwrap());
        assert_eq!(reference.observation_key, read.observations[0].context.identity.key().unwrap());
        assert_eq!(reference.capture, read.capture);
        assert_eq!(reference.identity, read.observations[0].context.identity);
        assert_eq!(serde_json::to_value(&finding.metrics).unwrap(), serde_json::to_value(&expected.metrics).unwrap());
    }
}

#[test]
fn the_same_hash_in_different_native_cache_entries_remains_separate() {
    let mut snapshot = snapshot();
    let read = &mut snapshot.pressure.as_mut().unwrap().grants[0];
    let mut other = read.observations[0].clone();
    other.context.identity.plan_handle = "0xAABB".into();
    other.context.identity.plan_generation = "2".into();
    read.observations.push(other);
    let SourceObservation::Snapshot(description) = &mut read.description.observation else { panic!() };
    description.records = 2;
    description.sources[0].records = 2;
    let expected: BTreeSet<_> =
        read.observations.iter().map(|row| format!("memory-grant:{}", read.entry_key(row).unwrap())).collect();
    assert!(read.is_valid());
    let findings = memory_grant(&snapshot);
    assert_eq!(findings.len(), 2);
    assert_eq!(findings.iter().map(|finding| finding.id.clone()).collect::<BTreeSet<_>>(), expected);
    assert!(findings.iter().all(|finding| finding.queries.is_empty()));
}

#[test]
fn overlapping_reads_do_not_sum_the_same_qualified_entries_counters() {
    let mut snapshot = snapshot();
    let mut again = snapshot.pressure.as_ref().unwrap().grants[0].clone();
    again.capture.observation_id = "grant-read-b".into();
    let SourceObservation::Snapshot(description) = &mut again.description.observation else { panic!() };
    description.observed_until = "2026-11-03T12:00:00.030Z".into();
    snapshot.pressure.as_mut().unwrap().grants.push(again);
    let findings = memory_grant(&snapshot);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].metrics.iter().find(|metric| metric.is("check.memory_grant.metric.runs")).unwrap().text.as_deref(),
        Some("40")
    );
    let view = statement_grants::explain(&snapshot.pressure.as_ref().unwrap().grants, &[]).unwrap();
    assert_eq!(view.reads.len(), 2);
    assert_eq!(view.entries.len(), 2);
    assert_ne!(view.entries[0].snapshot_key, view.entries[1].snapshot_key);
}

#[test]
fn an_unknown_member_preserves_positive_facts_without_associating_two_reads() {
    let mut snapshot = snapshot();
    let first = &mut snapshot.pressure.as_mut().unwrap().grants[0];
    first.description.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let mut again = first.clone();
    again.capture.observation_id = "grant-read-b".into();
    let SourceObservation::Snapshot(description) = &mut again.description.observation else { panic!() };
    description.observed_until = "2026-11-03T12:00:00.030Z".into();
    snapshot.pressure.as_mut().unwrap().grants.push(again);
    let findings = memory_grant(&snapshot);
    assert_eq!(findings.len(), 2);
    assert_ne!(findings[0].id, findings[1].id);
    assert!(
        findings
            .iter()
            .all(|finding| finding.actionability == Actionability::Information && finding.queries.is_empty())
    );
}

#[test]
fn a_later_balanced_execution_or_quiet_matching_query_does_not_erase_recorded_imbalance() {
    let mut snapshot = snapshot();
    let expected = memory_grant(&snapshot).remove(0);
    let row = &mut snapshot.pressure.as_mut().unwrap().grants[0].observations[0];
    row.last_grant_kibibytes = Some("102400".into());
    row.last_used_kibibytes = Some("61440".into());
    row.min_grant_kibibytes = Some("102400".into());
    snapshot.workload.as_mut().unwrap().queries[0].last_seen = Some("2020-01-01T00:00:00Z".into());
    let finding = memory_grant(&snapshot).remove(0);
    assert_eq!(finding.id, expected.id);
    assert_eq!(finding.title, expected.title);
    assert_eq!(finding.actionability, Actionability::Information);
    assert!(!finding.inactive);
    assert_eq!(finding.last_seen, None);
    snapshot.workload = None;
    let report = crate::analyze(snapshot);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.id == expected.id && finding.actionability == Actionability::Information)
    );
}

#[test]
fn imbalance_thresholds_use_exact_native_totals_and_do_not_pair_extrema() {
    let mut snapshot = snapshot();
    let row = &mut snapshot.pressure.as_mut().unwrap().grants[0].observations[0];
    row.executions = Some("5".into());
    row.total_grant_kibibytes = Some("256000".into());
    row.total_used_kibibytes = Some("64000".into());
    row.last_grant_kibibytes = None;
    row.last_used_kibibytes = None;
    row.min_grant_kibibytes = None;
    row.min_used_kibibytes = None;
    row.max_grant_kibibytes = None;
    row.max_used_kibibytes = None;
    assert_eq!(memory_grant(&snapshot).len(), 1);
    snapshot.pressure.as_mut().unwrap().grants[0].observations[0].total_used_kibibytes = Some("64001".into());
    assert!(memory_grant(&snapshot).is_empty());
    let row = &mut snapshot.pressure.as_mut().unwrap().grants[0].observations[0];
    row.total_used_kibibytes = None;
    row.max_grant_kibibytes = Some("1000000".into());
    row.min_used_kibibytes = Some("0".into());
    assert!(memory_grant(&snapshot).is_empty(), "independent extrema cannot manufacture paired totals");
}

#[test]
fn no_current_pressure_assessment_is_inferred_from_complete_or_failed_cache_reads() {
    let mut snapshot = snapshot();
    snapshot.workload = None;
    assert_eq!(memory_grant(&snapshot).len(), 1);
    let source = snapshot.sources.iter_mut().find(|source| source.source == source_id::GRANTS).unwrap();
    source.state = culprit_core::SourceState::Collected;
    source.reason = None;
    assert!(matches!(Check::evidence(&MemoryGrantCheck, &snapshot), culprit_core::CheckEvidence::Insufficient(_)));
    snapshot.pressure.as_mut().unwrap().grants.clear();
    assert!(matches!(Check::evidence(&MemoryGrantCheck, &snapshot), culprit_core::CheckEvidence::Insufficient(_)));
    let source = snapshot.sources.iter_mut().find(|source| source.source == source_id::GRANTS).unwrap();
    source.state = culprit_core::SourceState::Unavailable;
    source.reason = Some("Original cache source was denied".into());
    assert!(
        matches!(Check::evidence(&MemoryGrantCheck, &snapshot), culprit_core::CheckEvidence::Unavailable(reason) if reason == "Original cache source was denied")
    );
}
