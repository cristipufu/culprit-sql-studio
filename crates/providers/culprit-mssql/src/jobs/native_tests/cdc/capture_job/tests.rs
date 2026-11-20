use super::*;
use crate::{resources::test_support::replace, value::Val};

const JOB: &str = "abcdefab-2222-3333-4444-555555555555";
const NAME: &str = "cdc.owned_capture";
const ORIGINAL_AT: &str = "2026-11-20T00:00:00Z";

fn guard(phase: &str, at: &str) -> Rec {
    Rec::of(&[
        ("witness_phase", Val::Text(phase.into())),
        ("job_id", Val::Text(JOB.to_uppercase())),
        ("job_name", Val::Text(NAME.into())),
        ("database_id", Val::Int(7)),
        ("agent_session_id", Val::Int(3)),
        ("observed_at", Val::Text(at.into())),
        ("exact_owned_job", Val::Bool(true)),
        ("same_agent_session", Val::Bool(true)),
        ("agent_running", Val::Bool(true)),
        ("job_disabled", Val::Bool(true)),
        ("activity_row_present", Val::Bool(true)),
        // Actual owned failure: requested but never queued/started/stopped, while Agent reports IDLE.
        ("run_requested_without_offset", Val::Text("2026-11-20T00:00:00.000".into())),
        ("queued_without_offset", Val::Null),
        ("execution_started_without_offset", Val::Null),
        ("execution_stopped_without_offset", Val::Null),
        ("disable_return_code", Val::Int(0)),
    ])
}
fn original() -> JobGuard {
    JobGuard::read(&guard("disabled", ORIGINAL_AT), "disabled").unwrap()
}
fn runtime_row(status: Val) -> Rec {
    Rec::of(&[
        ("job_id", Val::Text(JOB.into())),
        ("name", Val::Text(NAME.into())),
        ("enabled", Val::Int(0)),
        ("current_execution_status", status),
        ("type", Val::Int(1)),
    ])
}
fn runtime_rows(status: i64) -> Vec<Rec> {
    vec![
        guard("before", "2026-11-20T00:00:00.100Z"),
        runtime_row(Val::Int(status)),
        guard("after", "2026-11-20T00:00:00.200Z"),
    ]
}
fn state(rows: &[Rec]) -> Result<RuntimeState> {
    let original = original();
    let runtime = Guarded::read(rows, JobRuntime::read)?;
    runtime.require_original(&original, &original.observed_at)?;
    runtime.value.state(&original.owner)
}

#[test]
fn requested_only_activity_and_native_runtime_idle_require_no_stop() {
    let rows = runtime_rows(4);
    assert_eq!(state(&rows).unwrap(), RuntimeState::Idle);
    let original = original();
    assert_eq!(original.owner.job_id, JOB, "native UUID case does not change identity");
    assert!(original.run_requested_without_offset.is_some());
    assert!(original.queued_without_offset.is_none());
    assert!(original.execution_started_without_offset.is_none());
    assert!(original.execution_stopped_without_offset.is_none());
    let witness = CaptureJobWitness::default();
    assert_eq!(witness.action(state(&rows).unwrap()), JobAction::Complete);
    assert!(!witness.stop_requested);
    // Missing activity is not itself idle; the same guarded public Agent runtime still supplies the proof.
    let mut no_activity = rows;
    for endpoint in [0, 2] {
        no_activity[endpoint] = replace(
            &no_activity[endpoint],
            &[("activity_row_present", Val::Bool(false)), ("run_requested_without_offset", Val::Null)],
        );
    }
    assert_eq!(state(&no_activity).unwrap(), RuntimeState::Idle);
}

#[test]
fn only_native_execution_may_request_one_stop_and_transitions_are_not_idle() {
    let mut witness = CaptureJobWitness::default();
    assert_eq!(state(&runtime_rows(1)).unwrap(), RuntimeState::Executing);
    assert_eq!(witness.action(RuntimeState::Executing), JobAction::Stop);
    for status in [2, 3, 5, 7] {
        let state = state(&runtime_rows(status)).unwrap();
        assert_eq!(state, RuntimeState::Transitional);
        assert_eq!(witness.action(state), JobAction::Observe);
    }
    witness.stop_requested = true;
    assert_eq!(witness.action(RuntimeState::Executing), JobAction::Observe);
    assert_eq!(witness.action(RuntimeState::Idle), JobAction::Complete);
    for status in [0, 6, 8, 99] {
        assert!(state(&runtime_rows(status)).is_err(), "{status}");
    }
    for status in [Val::Null, Val::Text("unknown".into()), Val::Int(-1)] {
        assert!(JobRuntime::read(&runtime_row(status)).is_err());
    }
}

#[test]
fn foreign_runtime_job_name_enabled_state_or_target_kind_never_supplies_idle() {
    for (column, value) in [
        ("job_id", Val::Text("abcdefab-2222-3333-4444-555555555556".into())),
        ("job_id", Val::Null),
        ("job_id", Val::Text("invalid-native-id".into())),
        ("job_id", Val::Text(tiberius::Uuid::nil().to_string())),
        ("name", Val::Text("cdc.foreign_capture".into())),
        ("enabled", Val::Int(1)),
        ("enabled", Val::Null),
        ("type", Val::Int(2)),
    ] {
        let mut rows = runtime_rows(4);
        rows[1] = replace(&rows[1], &[(column, value)]);
        assert!(state(&rows).is_err(), "{column}");
    }
}

#[test]
fn both_native_guards_require_original_job_database_all_steps_and_same_running_agent() {
    for endpoint in [0, 2] {
        for (column, value) in [
            ("job_id", Val::Text("abcdefab-2222-3333-4444-555555555556".into())),
            ("job_name", Val::Text("cdc.foreign_capture".into())),
            ("database_id", Val::Int(8)),
            ("database_id", Val::Int(2)),
            ("agent_session_id", Val::Int(4)),
            ("agent_session_id", Val::Int(0)),
            ("exact_owned_job", Val::Bool(false)),
            ("same_agent_session", Val::Bool(false)),
            ("agent_running", Val::Bool(false)),
            ("job_disabled", Val::Bool(false)),
            ("same_agent_session", Val::Null),
        ] {
            let mut rows = runtime_rows(4);
            rows[endpoint] = replace(&rows[endpoint], &[(column, value)]);
            assert!(state(&rows).is_err(), "endpoint={endpoint}, {column}");
        }
    }
}

#[test]
fn missing_duplicate_reordered_or_unmarked_results_cannot_supply_runtime() {
    let rows = runtime_rows(4);
    for altered in [
        Vec::new(),
        rows[..2].to_vec(),
        vec![rows[0].clone(), rows[0].clone(), rows[2].clone()],
        vec![rows[1].clone(), rows[0].clone(), rows[2].clone()],
        [rows.clone(), vec![rows[1].clone()]].concat(),
    ] {
        assert!(state(&altered).is_err());
    }
    let mut altered = rows;
    altered[2] = replace(&altered[2], &[("witness_phase", Val::Text("before".into()))]);
    assert!(state(&altered).is_err());
}

#[test]
fn native_guard_clocks_cannot_regress_from_the_original_or_previous_drained_phase() {
    for (endpoint, at) in [(0, "2026-11-19T23:59:59Z"), (2, "2026-11-20T00:00:00.050Z")] {
        let mut rows = runtime_rows(4);
        rows[endpoint] = replace(&rows[endpoint], &[("observed_at", Val::Text(at.into()))]);
        assert!(state(&rows).is_err());
    }
    let runtime = Guarded::read(&runtime_rows(4), JobRuntime::read).unwrap();
    assert!(runtime.require_original(&original(), "2026-11-20T00:00:00.150000000Z").is_err());
}

#[test]
fn a_stop_return_code_requires_original_guards_and_is_not_an_idle_witness() {
    let original = original();
    for code in [0, 1, 22022] {
        let rows = vec![
            guard("before", ORIGINAL_AT),
            Rec::of(&[("stop_return_code", Val::Int(code))]),
            guard("after", ORIGINAL_AT),
        ];
        let stopped = Guarded::read(&rows, |row| integer(row, "stop_return_code")).unwrap();
        assert_eq!(stopped.require_stop_ack(&original, &original.observed_at).is_ok(), code == 0);
    }
    let witness = CaptureJobWitness { stop_requested: true, ..Default::default() };
    assert_eq!(witness.action(RuntimeState::Executing), JobAction::Observe);
    assert_eq!(witness.action(RuntimeState::Transitional), JobAction::Observe);
    assert!(!witness.settled_idle);
}

#[tokio::test(start_paused = true)]
async fn native_runtime_observations_and_waits_do_not_extend_the_two_second_deadline() {
    let mut budget = SettlementBudget::new();
    assert_eq!(budget.next_observation().unwrap(), RUNTIME_SETTLEMENT_TIMEOUT);
    tokio::time::advance(RUNTIME_SETTLEMENT_TIMEOUT - Duration::from_millis(1)).await;
    assert_eq!(budget.next_observation().unwrap(), Duration::from_millis(1));
    let started = Instant::now();
    let error = budget.pause().await.unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_millis(1));
    assert_eq!(error.detail, CollectionStop::TimedOut.error().detail);
    assert!(budget.next_observation().is_err());
}

#[tokio::test(start_paused = true)]
async fn twentieth_pending_observation_refuses_another_read_or_wait() {
    let mut budget = SettlementBudget::new();
    for _ in 0..MAX_RUNTIME_OBSERVATIONS {
        assert!(budget.next_observation().is_ok());
    }
    assert_eq!(budget.next_observation().unwrap_err().detail, CollectionStop::SourceNotReady.error().detail);
    let started = Instant::now();
    assert!(budget.pause().await.is_err());
    assert!(started.elapsed().is_zero());
    let reported = limits();
    assert_eq!(reported["maximum_control_dispatches"], 22);
    assert_eq!(reported["maximum_runtime_procedure_calls"], 20);
    assert_eq!(reported["maximum_stop_requests"], 1);
    assert_eq!(reported["maximum_owner_guard_selects"], 43);
    assert!(RUNTIME_SETTLEMENT_TIMEOUT < CONTROL_TIMEOUT);
}

#[test]
fn client_read_runtime_is_bracketed_and_never_nested_insert_exec_or_request_inference() {
    let original = original();
    let runtime = runtime_sql(&original.owner);
    let before = runtime.find("N'before' AS witness_phase").unwrap();
    let read = runtime.find("EXEC msdb.dbo.sp_help_job @job_id=@job,@job_aspect=N'JOB'").unwrap();
    let after = runtime.find("N'after' AS witness_phase").unwrap();
    assert!(before < read && read < after);
    for sql in [disable_sql(), runtime, stop_sql(&original.owner)] {
        assert!(sql.contains("c.database_id=DB_ID() AND c.job_type=N'capture'"));
        assert!(sql.contains("j.name=N'cdc.'+DB_NAME()+N'_capture'"));
        assert!(sql.contains("s.subsystem=N'TSQL' AND s.database_name=DB_NAME()"));
        assert!(sql.contains("NOT EXISTS(SELECT 1 FROM msdb.dbo.sysjobsteps"));
        assert!(sql.contains("a.job_id=@job AND a.session_id=@agent"));
        assert!(!sql.contains("INSERT EXEC") && !sql.contains("INTO #"));
        assert!(!sql.contains("TRY") && !sql.contains("CATCH") && !sql.contains("ERROR_NUMBER"));
        assert!(!sql.contains("WAITFOR") && !sql.contains("WHILE"));
        assert!(!sql.contains("THEN 'Idle'") && !sql.contains("a.stop_execution_date IS NOT NULL THEN 1"));
        assert!(!sql.contains("sp_start_job") && !sql.contains("sp_cdc_add_job"));
    }
    assert_eq!(disable_sql().matches("sp_update_job").count(), 1);
    assert_eq!(stop_sql(&original.owner).matches("sp_stop_job").count(), 1);
}
