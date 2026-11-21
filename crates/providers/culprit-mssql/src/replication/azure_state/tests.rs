use super::super::authority::tests::{layout, owner};
use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
const LOCAL: &str = "91487244-730d-4b83-8e49-d482241c7288";
pub(crate) fn row() -> Rec {
    let base = super::super::tests::availability();
    let mut fields = base
        .columns()
        .filter(|(name, _)| {
            [
                "is_primary",
                "state",
                "native_state",
                "mode",
                "native_mode",
                "is_commit_participant",
                "database_state",
                "native_database_state",
                "is_suspended",
                "suspend_reason",
                "synchronization_health",
                "native_synchronization_health",
                "native_suspend_reason",
                "log_send_queue_kb",
                "redo_queue_kb",
                "log_send_rate_kb_per_second",
                "redo_rate_kb_per_second",
                "secondary_lag_seconds",
                "filestream_send_rate_kb_per_second",
                "low_water_mark_for_ghosts",
                "recovery_lsn",
                "truncation_lsn",
                "last_sent_lsn",
                "last_received_lsn",
                "last_hardened_lsn",
                "last_redone_lsn",
                "end_of_log_lsn",
                "last_commit_lsn",
                "last_sent_without_offset",
                "last_received_without_offset",
                "last_hardened_without_offset",
                "last_redone_without_offset",
                "last_commit_without_offset",
                "database_id",
                "group_id",
                "group_database_id",
                "replica_id",
                "is_local",
            ]
            .contains(name)
        })
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    fields.extend([
        ("selected_database_id", Val::Int(7)),
        ("observer_replica_id", Val::Text(LOCAL.into())),
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("state_present", Val::Int(1)),
    ]);
    replace(
        &Rec::of(&fields),
        &[
            ("replica_id", Val::Text(LOCAL.into())),
            ("database_id", Val::Int(7)),
            ("is_local", Val::Int(1)),
            ("group_id", Val::Null),
            ("group_database_id", Val::Null),
            ("mode", Val::Null),
            ("native_mode", Val::Null),
            ("redo_queue_kb", Val::Int(0)),
        ],
    )
}
#[tokio::test]
async fn original_local_rows_preserve_null_group_ids_and_exact_progress_without_ag_catalog_or_quorum_claims() {
    let owner = owner().await;
    let layout = layout(&owner).await;
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let (part, at) = read(&mut reader, &owner, &layout, 1).await.unwrap();
    assert!(at.is_some());
    let Part::Observed { facts, complete, .. } = part else { panic!() };
    assert_eq!(facts.len(), 1);
    assert!(!complete);
    let ReplicationFact::AzureReplicaState(point) = &facts[0] else { panic!() };
    assert!(point.group_id.is_none() && point.group_database_id.is_none());
    assert_eq!(point.progress.log_send_queue_kb.as_deref(), Some("9007199254740993"));
    assert_eq!(point.progress.redo_queue_kb.as_deref(), Some("0"));
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.calls[0].limit, 2);
    let sql = &reader.calls[0].sql;
    assert!(sql.contains("database_id=@database_id AND is_local=1 AND replica_id=@observer"));
    assert!(sql.contains("@database_id int=DB_ID()"));
    assert!(!sql.contains("quorum_"));
    assert!(!sql.contains("SELECT TOP(2) *"));
    assert!(!sql.contains("sys.availability_groups"));
    assert!(!sql.contains("sys.availability_replicas"));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn foreign_native_database_replica_role_clock_and_ambiguous_local_rows_refuse_publication() {
    let owner = owner().await;
    let layout = layout(&owner).await;
    for change in [
        "database_id",
        "selected_database_id",
        "replica_id",
        "observer_replica_id",
        "is_local",
        "updateability",
        "observed_at",
    ] {
        let value = match change {
            "database_id" | "selected_database_id" => Val::Int(8),
            "is_local" => Val::Int(0),
            "updateability" => Val::Text("READ_ONLY".into()),
            "observed_at" => Val::Text("2019-01-01T00:00:00Z".into()),
            _ => Val::Text("11111111-1111-1111-1111-111111111111".into()),
        };
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(change, value)])])]);
        assert!(read(&mut reader, &owner, &layout, 1).await.is_err(), "{change}");
        assert_eq!(reader.queries(), 1);
    }
    let mut reader = FakeRead::new(vec![Ok(vec![row(), row()])]);
    assert!(read(&mut reader, &owner, &layout, 1).await.is_err());
}
#[tokio::test]
async fn native_empty_is_a_visibility_limited_read_and_null_progress_does_not_become_zero_or_absence() {
    let owner = owner().await;
    let layout = layout(&owner).await;
    let row = row();
    let values = row
        .columns()
        .map(|(name, value)| {
            let value =
                if matches!(name, "selected_database_id" | "observer_replica_id" | "observed_at" | "updateability") {
                    value.clone()
                } else if name == "state_present" {
                    Val::Int(0)
                } else {
                    Val::Null
                };
            (name, value)
        })
        .collect::<Vec<_>>();
    let empty = Rec::of(&values);
    let mut reader = FakeRead::new(vec![Ok(vec![empty.clone()])]);
    let (part, at) = read(&mut reader, &owner, &layout, 1).await.unwrap();
    assert!(at.is_some());
    let (rows, coverage) = selection::SelectedRead { parts: vec![part] }.into_records(true, None);
    assert!(rows.is_empty());
    assert_eq!(coverage[0].state, culprit_core::SourceState::Partial);
    assert!(!coverage[0].complete_scope);
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&empty, &[("redo_queue_kb", Val::Int(0))])])]);
    assert!(read(&mut reader, &owner, &layout, 1).await.is_err());
}
#[tokio::test]
async fn permission_layout_and_caller_exhaustion_remain_independent_but_terminal_errors_never_continue() {
    let owner = owner().await;
    let mut layout = layout(&owner).await;
    let mut reader = FakeRead::new(vec![Ok(vec![super::super::tests::unavailable(229)])]);
    assert!(matches!(read(&mut reader, &owner, &layout, 1).await.unwrap().0, Part::Unavailable { .. }));
    assert_eq!(reader.queries(), 1);
    layout.local_state_columns = 0;
    let mut reader = FakeRead::new(vec![]);
    assert!(matches!(read(&mut reader, &owner, &layout, 1).await.unwrap().0, Part::Unavailable { .. }));
    assert_eq!(reader.queries(), 0);
    let mut reader = FakeRead::new(vec![]);
    assert!(matches!(read(&mut reader, &owner, &layout, 0).await.unwrap().0, Part::Unavailable { .. }));
    assert_eq!(reader.queries(), 0);
    layout.local_state_columns = authority::LOCAL_STATE_COLUMNS;
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
    ] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        assert_eq!(CollectionStop::from_error(&read(&mut reader, &owner, &layout, 1).await.err().unwrap()), stop);
        assert_eq!(reader.queries(), 1);
    }
}
