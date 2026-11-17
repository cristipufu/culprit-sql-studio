use super::*;

#[test]
fn documented_concurrent_phases_classify_the_original_maintenance_waiter_only() {
    for (command, native, phase) in [
        (
            MaintenanceCommand::CreateIndexConcurrently,
            "waiting for writers before build",
            PostgresIndexWaitPhase::WritersBeforeBuild,
        ),
        (
            MaintenanceCommand::ReindexConcurrently,
            "waiting for writers before validation",
            PostgresIndexWaitPhase::WritersBeforeValidation,
        ),
        (
            MaintenanceCommand::CreateIndexConcurrently,
            "waiting for old snapshots",
            PostgresIndexWaitPhase::OldSnapshots,
        ),
        (
            MaintenanceCommand::ReindexConcurrently,
            "waiting for readers before marking dead",
            PostgresIndexWaitPhase::ReadersBeforeMarkingDead,
        ),
        (
            MaintenanceCommand::ReindexConcurrently,
            "waiting for readers before dropping",
            PostgresIndexWaitPhase::ReadersBeforeDropping,
        ),
    ] {
        let mut row = snapshot().observations.remove(0);
        row.command = command;
        row.phase = native.into();
        let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &mut row.work else { panic!() };
        progress.current_locker_pid = Some("91".into());
        let original = row.clone();
        let wait = row.postgres_index_wait().unwrap();
        assert_eq!(wait.phase, phase);
        assert_eq!(wait.unresolved_locker_pid.as_deref(), Some("91"));
        assert_eq!(row, original, "classification never rewrites native facts");
        let json = serde_json::to_value(&wait).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2, "no owner SQL, generation, wait duration or completion");
        assert_eq!(serde_json::from_value::<PostgresIndexBuildWait>(json).unwrap(), wait);
    }
}

#[test]
fn incompatible_command_unknown_phase_and_other_native_progress_do_not_establish_waiting() {
    for (command, phase) in [
        (MaintenanceCommand::CreateIndex, "waiting for writers before build"),
        (MaintenanceCommand::Reindex, "waiting for old snapshots"),
        (MaintenanceCommand::CreateIndexConcurrently, "waiting for readers before dropping"),
        (MaintenanceCommand::CreateIndexConcurrently, "waiting for readers before marking dead"),
        (MaintenanceCommand::CreateIndexConcurrently, "building index: scanning table"),
        (MaintenanceCommand::ReindexConcurrently, "waiting for an undocumented resource"),
    ] {
        let mut row = snapshot().observations.remove(0);
        row.command = command;
        row.phase = phase.into();
        assert!(row.is_valid(), "unsupported native phase stays a usable raw point");
        assert!(row.postgres_index_wait().is_none(), "{command:?}: {phase}");
    }
    let mut row = snapshot().observations.remove(0);
    row.command = MaintenanceCommand::CreateIndexConcurrently;
    row.phase = "waiting for writers before build".into();
    row.work = MaintenanceProgressWork::YsqlIndexBuild { progress: Box::default() };
    assert!(row.is_valid());
    assert!(row.postgres_index_wait().is_none(), "YSQL raw text does not acquire PostgreSQL semantics");
}

#[test]
fn zero_absent_or_invalid_locker_does_not_invent_an_owner() {
    let mut row = snapshot().observations.remove(0);
    row.command = MaintenanceCommand::CreateIndexConcurrently;
    row.phase = "waiting for old snapshots".into();
    for pid in [None, Some("0")] {
        let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &mut row.work else { panic!() };
        progress.current_locker_pid = pid.map(str::to_owned);
        assert_eq!(row.postgres_index_wait().unwrap().unresolved_locker_pid, None);
    }
    let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &mut row.work else { panic!() };
    progress.current_locker_pid = Some("0091".into());
    assert!(!row.is_valid());
    assert!(row.postgres_index_wait().is_none());
}
