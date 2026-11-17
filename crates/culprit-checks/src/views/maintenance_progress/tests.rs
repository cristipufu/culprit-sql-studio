use super::*;
use culprit_core::{DatabaseIdentity, Engine, SourceState};

mod waits;

fn snapshot() -> MaintenanceProgressSnapshot {
    MaintenanceProgressSnapshot {
        source: "pg_stat_progress_create_index".into(),
        context: NativeReadContext {
            target: DatabaseIdentity::server(Engine::Postgres, "shop", "fixture.invalid", 5432, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Postgres,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-cluster"),
                    native: NativeIdentity::new("native-member"),
                }),
                epoch: Some("native-process".into()),
            },
            observed_from: "2026-11-15T10:00:00Z".into(),
            observed_until: "2026-11-15T10:00:01Z".into(),
            database_writable: Some(true),
        },
        coverage: StateSourceCoverage::observed("pg_stat_progress_create_index".into(), 1, false),
        result: FocusedRequestResult {
            focus: None,
            database_writable: Some(true),
            binding: FocusedRequestBinding::Independent,
            membership: FocusedRequestMembership::Observed,
        },
        observations: vec![MaintenanceProgressObservation {
            observed_at: "2026-11-15T10:00:00.5Z".into(),
            actor: ActivityKey {
                session: NativeActivityIdentity { id: "42".into(), generation: "backend".into() },
                request: Some(NativeActivityIdentity { id: "statement".into(), generation: "request".into() }),
                transaction: None,
            },
            statement: Some(NativeIdentity::new("native-statement")),
            database_oid: "16384".into(),
            relation_oid: "16420".into(),
            index_oid: Some("0".into()),
            command: MaintenanceCommand::CreateIndex,
            phase: "building index: scanning table".into(),
            work: MaintenanceProgressWork::PostgreSqlIndexBuild {
                progress: Box::new(PostgresIndexBuildProgress {
                    blocks_done: Some("9007199254740993".into()),
                    blocks_total: None,
                    current_locker_pid: Some("91".into()),
                    ..Default::default()
                }),
            },
        }],
    }
}

#[test]
fn exact_phase_counts_and_unqualified_locker_pid_do_not_become_resource_or_owner_claims() {
    let report = explain(&snapshot(), true).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.metrics[0].text.as_deref(), Some(format::digits("9007199254740993".into()).as_str()));
    assert!(entry.index.is_none());
    assert!(report.limitations.contains(&t!("view.maintenance_progress.limit.locker")));
    assert_eq!(entry.actor.session.id, "42");
    let json = serde_json::to_value(&report).unwrap();
    for missing in ["owner_sql", "resource_bytes", "completed_at", "percent"] {
        assert!(json.get(missing).is_none() && json["entries"][0].get(missing).is_none(), "{missing}");
    }
}

#[test]
fn ysql_table_backfill_estimate_is_not_a_postgresql_heap_fraction() {
    let mut input = snapshot();
    input.context.target = DatabaseIdentity::server(Engine::Yugabyte, "shop", "fixture.invalid", 5433, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut input.context.partition else { panic!() };
    member.engine = Engine::Yugabyte;
    member.kind = SharedResourceKind::Node;
    input.context.database_writable = None;
    input.result.database_writable = None;
    input.observations[0].work = MaintenanceProgressWork::YsqlIndexBuild {
        progress: Box::new(YsqlIndexBuildProgress {
            tuples_done: Some("9007199254740993".into()),
            tuples_total_estimate: Some("42".into()),
        }),
    };
    let report = explain(&input, true).unwrap();
    assert_eq!(report.entries[0].metrics.len(), 2);
    assert_eq!(report.entries[0].metrics[0].text.as_deref(), Some(format::digits("9007199254740993".into()).as_str()));
    assert!(report.limitations.contains(&t!("view.maintenance_progress.limit.ysql")));
    assert!(report.entries[0].metrics.iter().all(|metric| matches!(metric.unit, culprit_core::Unit::Text)));
}

#[test]
fn warehouse_gaps_preserve_native_roster_and_positive_membership_without_a_fabricated_row() {
    let mut input = snapshot();
    input.observations.clear();
    let report = explain(&input, false).unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert_eq!(report.membership, FocusedRequestMembership::Observed);
    assert!(report.entries.is_empty());
    assert!(report.limitations.iter().any(|text| text.contains("readback")));
    assert!(explain(&input, true).is_err());
}

#[test]
fn missing_progress_and_unavailable_source_are_distinct_from_completion() {
    let mut input = snapshot();
    input.observations.clear();
    input.coverage = StateSourceCoverage::observed(input.source.clone(), 0, false);
    input.result.membership = FocusedRequestMembership::NotObserved;
    let absent = explain(&input, true).unwrap();
    assert!(absent.summary.contains("does not establish completion"));
    input.coverage =
        StateSourceCoverage::unavailable(input.source.clone(), culprit_core::ErrorCode::PermissionDenied, None);
    input.result.membership = FocusedRequestMembership::Unavailable;
    let unavailable = explain(&input, true).unwrap();
    assert_eq!(unavailable.source_state, SourceState::Unavailable);
    assert_ne!(absent.summary, unavailable.summary);
}

#[test]
fn native_null_counters_are_omitted_and_zero_remains_a_recorded_value() {
    let mut input = snapshot();
    let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &mut input.observations[0].work else { panic!() };
    **progress = PostgresIndexBuildProgress { blocks_done: Some("0".into()), ..Default::default() };
    let report = explain(&input, true).unwrap();
    assert_eq!(report.entries[0].metrics.len(), 1);
    assert_eq!(report.entries[0].metrics[0].text.as_deref(), Some("0"));
}

#[test]
fn future_phase_clock_and_inconsistent_native_counts_are_rejected() {
    let mut input = snapshot();
    input.observations[0].observed_at = "2026-11-15T10:00:02Z".into();
    assert!(explain(&input, true).is_err());
    input = snapshot();
    input.coverage.records = 0;
    assert!(explain(&input, false).is_err());
}
