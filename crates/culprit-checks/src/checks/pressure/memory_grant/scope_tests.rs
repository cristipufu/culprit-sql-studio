use super::*;
use culprit_core::{
    RuntimePopulation, RuntimePopulationCatalog, SnapshotOptions, i18n::Message, telemetry::NativeIdentity,
};

fn snapshot() -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = None;
    workload.population_catalog = Some(RuntimePopulationCatalog {
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: false,
        requires_selection: false,
        populations: vec![],
    });
    workload.windowed = true;
    workload.window_hours = 48.0;
    workload.window_start = Some("2026-11-01T00:00:00Z".into());
    workload.window_end = Some("2026-11-03T00:00:00Z".into());
    workload.total_time_ms = 80_000.0;
    workload.hourly_time_ms = None;
    workload.timeline = None;
    workload.queries.truncate(1);
    let query = &mut workload.queries[0];
    query.id = "original-runtime-query".into();
    query.query_hash = Some("0xABC".into());
    query.executions = 40.0;
    query.total_time_ms = 40_000.0;
    query.mean_time_ms = 1000.0;
    query.first_seen = workload.window_start.clone();
    query.last_seen = workload.window_end.clone();
    query.trend = None;
    query.waits =
        vec![culprit_core::WaitStat { category: "Memory".into(), wait_type: None, wait_ms: 8_000.0, tasks: None }];
    snapshot.options = Some(SnapshotOptions::default());
    snapshot.pressure = Some(Pressure {
        grants: vec![GrantStat {
            query_hash: "0xabc".into(),
            executions: 40.0,
            avg_granted_kb: 2.0 * 1024.0 * 1024.0,
            avg_used_kb: 40.0 * 1024.0,
            max_granted_kb: 3.0 * 1024.0 * 1024.0,
            last_granted_kb: None,
            last_used_kb: None,
        }],
        ..Default::default()
    });
    snapshot
}

fn scoped(snapshot: &mut Snapshot, group: &str, automatic: bool) {
    let population = NativeIdentity::new(group);
    let workload = snapshot.workload.as_mut().unwrap();
    workload.population = Some(population.clone());
    workload.population_catalog = Some(RuntimePopulationCatalog {
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: true,
        requires_selection: !automatic,
        populations: if automatic { vec![group] } else { vec!["1", "7"] }
            .into_iter()
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from("native group"),
                native_properties: Default::default(),
            })
            .collect(),
    });
    snapshot.options.as_mut().unwrap().workload_population = (!automatic).then_some(population);
}

#[test]
fn unpartitioned_grants_keep_the_matching_query_and_its_measured_memory_waits() {
    let snapshot = snapshot();
    let findings = memory_grant(&snapshot);
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.queries[0].id, "original-runtime-query");
    assert_eq!(finding.id, "memory-grant:original-runtime-query");
    assert!((finding.impact.db_time_share.unwrap() - 0.1).abs() < 1e-9);
    assert_eq!(finding.metrics.iter().find(|metric| metric.is("check.memory_grant.metric.runs")).unwrap().value, 40.0);
}

#[test]
fn the_same_hash_cannot_borrow_any_selected_or_automatically_selected_groups_runtime() {
    let original = snapshot();
    let unpartitioned = memory_grant(&original).remove(0);
    assert!(unpartitioned.impact.db_time_share.is_some(), "the control must exercise the prohibited wait/share join");
    for (group, automatic) in [("1", false), ("7", false), ("7", true)] {
        let mut snapshot = original.clone();
        scoped(&mut snapshot, group, automatic);
        let findings = memory_grant(&snapshot);
        assert_eq!(findings.len(), 1, "the independent positive native grant metrics remain visible");
        let finding = &findings[0];
        assert_eq!(finding.id, "memory-grant:0xabc");
        assert!(finding.queries.is_empty());
        assert_eq!(finding.impact.db_time_share, None);
        assert_eq!(finding.impact.time_ms_per_day, None);
        assert_eq!(
            serde_json::to_value(&finding.metrics).unwrap(),
            serde_json::to_value(&unpartitioned.metrics).unwrap()
        );
        assert_eq!(finding.severity, unpartitioned.severity);
    }
}

#[test]
fn an_unscoped_grant_cannot_inherit_the_selected_queries_quiet_history() {
    let mut snapshot = snapshot();
    let query = &mut snapshot.workload.as_mut().unwrap().queries[0];
    query.last_seen = Some("2026-11-01T12:00:00Z".into());
    assert!(
        crate::all_stopped(&snapshot, &[&snapshot.queries()[0]]),
        "the matching runtime query must genuinely be quiet"
    );
    let historical = memory_grant(&snapshot).remove(0);
    scoped(&mut snapshot, "7", false);
    let independent = memory_grant(&snapshot).remove(0);
    assert!(independent.queries.is_empty());
    assert_ne!(independent.title, historical.title);
    assert_eq!(
        independent.title,
        t!("check.memory_grant.title.unknown", granted = kb(2.0 * 1024.0 * 1024.0), used = kb(40.0 * 1024.0))
    );
    assert_eq!(independent.impact.db_time_share, None);
}
