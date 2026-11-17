use super::*;
use crate::progress::{capture_from, test_support::*};

fn waiting(command: &str, phase: &str, pid: Option<&str>, changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = vec![
        ("command", Some(command)),
        ("phase", Some(phase)),
        ("index_oid", Some("16421")),
        ("blocks_total", None),
        ("blocks_done", None),
        ("tuples_total", None),
        ("tuples_done", None),
        ("lockers_total", Some("9007199254740993")),
        ("lockers_done", Some("0")),
        ("current_locker_pid", pid),
    ];
    fields.extend_from_slice(changes);
    native_progress(&fields)
}

#[test]
fn native_decoder_retains_exact_wait_code_and_uses_the_common_typed_interpretation() {
    let key = original_focus().original.key;
    for (command, phase, expected) in [
        ("CREATE INDEX CONCURRENTLY", "waiting for writers before build", PostgresIndexWaitPhase::WritersBeforeBuild),
        (
            "CREATE INDEX CONCURRENTLY",
            "waiting for writers before validation",
            PostgresIndexWaitPhase::WritersBeforeValidation,
        ),
        ("CREATE INDEX CONCURRENTLY", "waiting for old snapshots", PostgresIndexWaitPhase::OldSnapshots),
        (
            "REINDEX CONCURRENTLY",
            "waiting for readers before marking dead",
            PostgresIndexWaitPhase::ReadersBeforeMarkingDead,
        ),
        ("REINDEX CONCURRENTLY", "waiting for readers before dropping", PostgresIndexWaitPhase::ReadersBeforeDropping),
    ] {
        let point = decode(&waiting(command, phase, Some("91"), &[]), "16384", &key).unwrap();
        assert_eq!(point.actor, key);
        assert_eq!(point.phase, phase);
        assert_eq!(point.postgres_index_wait().unwrap().phase, expected);
        assert_eq!(point.postgres_index_wait().unwrap().unresolved_locker_pid.as_deref(), Some("91"));
        let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &point.work else { panic!() };
        assert_eq!(progress.lockers_total.as_deref(), Some("9007199254740993"));
        assert_eq!(progress.lockers_done.as_deref(), Some("0"));
    }
}

#[test]
fn native_wait_phase_cannot_override_database_or_backend_and_request_generations() {
    let expected = original_focus().original.key;
    for (field, value) in [
        ("database_oid", "16385"),
        ("backend_start", "2026-11-15T08:00:00.000002Z"),
        ("query_start", "2026-11-15T09:00:00.000003Z"),
    ] {
        let native = waiting(
            "CREATE INDEX CONCURRENTLY",
            "waiting for writers before build",
            Some("91"),
            &[(field, Some(value))],
        );
        assert!(decode(&native, "16384", &expected).is_err(), "{field}");
    }
}

#[tokio::test]
async fn wait_explanation_uses_the_unchanged_generation_guarded_read_and_no_extra_native_phase() {
    let mut io = progress_reader(
        180_000,
        0,
        Some(waiting("CREATE INDEX CONCURRENTLY", "waiting for writers before build", Some("91"), &[])),
    );
    let captured = capture_from(
        &mut io,
        "fixture.invalid",
        9,
        "42",
        Some(&original_focus()),
        &crate::progress::bounded_budget(&CollectionBudget::default()),
    )
    .await
    .unwrap();
    assert_eq!(io.calls.len(), 7);
    assert!(io.replies.is_empty());
    assert_eq!(io.calls.iter().filter(|sql| sql.contains("FROM pg_catalog.pg_stat_progress_create_index")).count(), 1);
    assert_eq!(captured.result.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert_eq!(
        captured.observations[0].postgres_index_wait().unwrap().phase,
        PostgresIndexWaitPhase::WritersBeforeBuild
    );
    assert!(io.calls[3].contains("a.backend_start=") && io.calls[3].contains("a.query_start="));
}
