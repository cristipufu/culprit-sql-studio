use super::*;

fn waiting(command: MaintenanceCommand, phase: &str) -> MaintenanceProgressSnapshot {
    let mut input = snapshot();
    input.observations[0].command = command;
    input.observations[0].phase = phase.into();
    input
}

#[test]
fn native_waiting_phases_explain_maintenance_as_the_affected_request_with_exact_original_counters() {
    for (native, phase, explanation, investigation) in [
        (
            "waiting for writers before build",
            PostgresIndexWaitPhase::WritersBeforeBuild,
            t!("view.maintenance_progress.wait.writers_before_build"),
            t!("view.maintenance_progress.wait.investigate_writers"),
        ),
        (
            "waiting for writers before validation",
            PostgresIndexWaitPhase::WritersBeforeValidation,
            t!("view.maintenance_progress.wait.writers_before_validation"),
            t!("view.maintenance_progress.wait.investigate_writers"),
        ),
        (
            "waiting for old snapshots",
            PostgresIndexWaitPhase::OldSnapshots,
            t!("view.maintenance_progress.wait.old_snapshots"),
            t!("view.maintenance_progress.wait.investigate_snapshots"),
        ),
        (
            "waiting for readers before marking dead",
            PostgresIndexWaitPhase::ReadersBeforeMarkingDead,
            t!("view.maintenance_progress.wait.readers_before_marking_dead"),
            t!("view.maintenance_progress.wait.investigate_readers"),
        ),
        (
            "waiting for readers before dropping",
            PostgresIndexWaitPhase::ReadersBeforeDropping,
            t!("view.maintenance_progress.wait.readers_before_dropping"),
            t!("view.maintenance_progress.wait.investigate_readers"),
        ),
    ] {
        let input = waiting(MaintenanceCommand::ReindexConcurrently, native);
        let original = input.clone();
        let report = explain(&input, true).unwrap();
        let entry = &report.entries[0];
        assert_eq!(entry.phase, native);
        assert_eq!(entry.wait.as_ref().unwrap().phase, phase);
        assert_eq!(entry.wait.as_ref().unwrap().unresolved_locker_pid.as_deref(), Some("91"));
        assert_eq!(entry.explanation, explanation);
        assert_eq!(entry.investigation, investigation);
        assert_eq!(entry.verify, t!("view.maintenance_progress.wait.verify"));
        assert_eq!(entry.metrics[0].text.as_deref(), Some("9007199254740993"));
        assert_eq!(entry.actor, original.observations[0].actor);
        assert_eq!(report.binding, FocusedRequestBinding::Independent);
        assert_eq!(input, original, "explanation must not repair or relabel native evidence");
        assert!(report.limitations.contains(&t!("view.maintenance_progress.limit.locker")));
    }
}

#[test]
fn working_unknown_or_incompatible_native_phase_retains_the_existing_noncausal_explanation() {
    for (command, phase) in [
        (MaintenanceCommand::CreateIndex, "waiting for writers before build"),
        (MaintenanceCommand::CreateIndexConcurrently, "waiting for readers before dropping"),
        (MaintenanceCommand::ReindexConcurrently, "building index: scanning table"),
        (MaintenanceCommand::ReindexConcurrently, "waiting for an undocumented resource"),
    ] {
        let report = explain(&waiting(command, phase), true).unwrap();
        let entry = &report.entries[0];
        assert!(entry.wait.is_none());
        assert_eq!(entry.phase, phase);
        assert_eq!(entry.explanation, t!("view.maintenance_progress.entry.postgres"));
        assert_eq!(entry.investigation, t!("view.maintenance_progress.investigate"));
        assert_eq!(entry.verify, t!("view.maintenance_progress.verify"));
    }
}

#[test]
fn missing_warehouse_point_and_native_disappearance_never_manufacture_a_wait_or_completion() {
    let mut input = waiting(MaintenanceCommand::CreateIndexConcurrently, "waiting for writers before build");
    input.observations.clear();
    let gap = explain(&input, false).unwrap();
    assert!(gap.entries.is_empty());
    assert_eq!(gap.membership, FocusedRequestMembership::Observed);
    assert_eq!(gap.native_rows, 1);
    assert!(gap.limitations.contains(&t!("view.maintenance_progress.limit.read")));
    assert!(explain(&input, true).is_err());
    input.result.membership = FocusedRequestMembership::NotObserved;
    input.coverage = StateSourceCoverage::observed(input.source.clone(), 0, false);
    let absent = explain(&input, true).unwrap();
    assert!(absent.entries.is_empty());
    assert_eq!(absent.summary, t!("view.maintenance_progress.summary.not_observed"));
}
