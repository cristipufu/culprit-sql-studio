use super::*;
use crate::{read::QueryRead, rows::Row};
use std::collections::VecDeque;

struct Read {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<(String, usize)>,
}
#[async_trait::async_trait]
impl QueryRead for Read {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls.push((sql.into(), limit));
        self.replies.pop_front().expect("only the bounded baseline native reads")
    }
}
fn metadata(at: &str, started: &str, visible: bool) -> Row {
    Row::from_fields(
        [
            ("build", Some("PostgreSQL 18.6".into())),
            ("database_oid", Some("42".into())),
            ("database", Some("shop".into())),
            ("version", Some("180006".into())),
            ("block_bytes", Some("8192".into())),
            ("standby", Some("f".into())),
            ("started_at", Some(started.into())),
            ("observed_at", Some(at.into())),
            ("full_visibility", Some(if visible { "t" } else { "f" }.into())),
            ("pgss_schema", None),
            ("pgss_columns", None),
            ("pgss_info", Some("f".into())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
fn actor(pid: &str, state: &str, backend: Option<&str>) -> Row {
    actor_with_wait(pid, state, backend, Some(("Lock", "transactionid")), "100")
}
fn actor_with_wait(pid: &str, state: &str, backend: Option<&str>, wait: Option<(&str, &str)>, elapsed_ms: &str) -> Row {
    Row::from_fields(
        [
            ("observed_at", Some("2020-01-01T06:00:00.100000Z".into())),
            ("pid", Some(pid.into())),
            ("backend_start", backend.map(str::to_owned)),
            ("query_start", Some("2020-01-01T06:00:00Z".into())),
            ("transaction_start", None),
            ("database_oid", Some("42".into())),
            ("database", Some("shop".into())),
            ("state", Some(state.into())),
            ("wait_event_type", wait.map(|(kind, _)| kind.into())),
            ("wait_event", wait.map(|(_, event)| event.into())),
            ("elapsed_ms", Some(elapsed_ms.into())),
            ("transaction_ms", None),
            ("idle_ms", Some("0".into())),
            ("query_id", None),
            ("application_name", Some("app".into())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
fn request(limit: u32) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget { max_snapshot_rows: limit, ..Default::default() },
    }
}
fn reader(rows: Vec<Row>, visible: bool) -> Read {
    Read {
        replies: vec![
            Ok(vec![metadata("2020-01-01T06:00:00Z", "2020-01-01T05:00:00Z", visible)]),
            Ok(rows),
            Ok(vec![metadata("2020-01-01T06:00:00.200000Z", "2020-01-01T05:00:00Z", visible)]),
        ]
        .into(),
        calls: vec![],
    }
}
#[tokio::test]
async fn metadata_baseline_keeps_short_waits_and_ordinary_backends_before_any_optional_reads() {
    let mut reader = reader(
        vec![actor("11", "active", Some("2020-01-01T05:30:00Z")), actor("12", "idle", Some("2020-01-01T05:31:00Z"))],
        true,
    );
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let native = capture.description.observation.activity().unwrap();
    assert!(native.is_valid() && native.coverage.complete_scope);
    assert_eq!(native.observations, 2);
    assert_eq!(native.population, ActivityPopulation::ClientBackends);
    assert_eq!(reader.calls.len(), 3);
    assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_blocking_pids")
        && !sql.contains("pg_locks")
        && !sql.contains("a.query ELSE")));
    let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
    assert_eq!(row.elapsed_ms.as_deref(), Some("100"));
    assert_eq!(row.wait.as_ref().unwrap().native_type.as_deref(), Some("Lock: transactionid"));
    assert!(row.wait.as_ref().unwrap().owners.is_empty());
    assert_eq!(row.blocking_status, ActivityBlockingStatus::Unknown);
    assert_eq!(row.waiting_on_lock, Some(true));
    assert!(row.statement_identity.is_some());
    assert!(matches!(row.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }));
}
#[tokio::test]
async fn a_native_lock_point_at_forty_seconds_needs_neither_an_owner_walk_nor_an_invented_wait_duration() {
    let row = actor_with_wait("11", "active", Some("2020-01-01T05:30:00Z"), Some(("Lock", "transactionid")), "40000");
    let mut reader = reader(vec![row], true);
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
    let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
    assert_eq!(row.waiting_on_lock, Some(true));
    assert_eq!(row.elapsed_ms.as_deref(), Some("40000"));
    assert!(row.wait.as_ref().unwrap().elapsed_ms.is_none());
    assert!(row.wait.as_ref().unwrap().owners.is_empty());
    assert_eq!(row.blocking_status, ActivityBlockingStatus::Unknown);
    assert!(!row.conditions(&ActivityPolicy::default()).contains(&ActivityCondition::Blocked));
    assert_eq!(reader.calls.len(), 3);
    assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_blocking_pids")
        && !sql.contains("pg_locks")
        && !sql.contains("a.query ELSE")));
}

#[tokio::test]
async fn unknown_extension_and_other_native_waits_are_not_a_proven_negative_lock_inventory() {
    for wait in [None, Some(("Extension", "DocDBWait")), Some(("IO", "DataFileRead")), Some(("Client", "ClientRead"))] {
        let row = actor_with_wait("11", "active", Some("2020-01-01T05:30:00Z"), wait, "40000");
        let mut reader = reader(vec![row], true);
        let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
        let TelemetryRecord::Activity(row) = &capture.records[0] else { panic!() };
        assert_eq!(row.waiting_on_lock, None, "{wait:?}");
        assert!(row.wait.as_ref().is_none_or(|wait| wait.elapsed_ms.is_none()));
        assert_eq!(reader.calls.len(), 3);
    }
}
#[tokio::test]
async fn a_drained_row_witness_retains_the_prefix_with_partial_native_coverage() {
    let mut reader = reader(
        vec![actor("11", "active", Some("2020-01-01T05:30:00Z")), actor("12", "active", Some("2020-01-01T05:31:00Z"))],
        true,
    );
    let capture = capture(&mut reader, "fixture.invalid", 5432, &request(1)).await.unwrap();
    let native = capture.description.observation.activity().unwrap();
    assert!(native.is_valid());
    assert_eq!(capture.records.len(), 1);
    assert_eq!(native.native_rows, 2);
    assert_eq!(native.coverage.stopped, Some(CollectionStop::RowLimit));
    assert!(!native.coverage.complete_scope);
    assert_eq!(reader.calls[1].1, 2);
    assert_eq!(reader.calls.len(), 3, "no optional lock or payload work precedes the baseline");
}
#[tokio::test]
async fn hidden_or_disabled_rows_cannot_become_fabricated_idle_sessions_or_complete_absence() {
    for state in ["client backend", "disabled"] {
        let mut reader =
            reader(vec![actor("11", state, None), actor("12", "active", Some("2020-01-01T05:31:00Z"))], false);
        let capture = capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.unwrap();
        let native = capture.description.observation.activity().unwrap();
        assert_eq!(capture.records.len(), 1);
        assert_eq!(native.native_rows, 2);
        assert_eq!(native.coverage.error_code, Some(ErrorCode::PermissionDenied));
        assert_eq!(native.coverage.state, culprit_core::SourceState::Partial);
        assert!(!native.coverage.complete_scope);
        let TelemetryRecord::Activity(visible) = &capture.records[0] else { panic!() };
        assert_eq!(visible.waiting_on_lock, Some(true), "hidden peers cannot erase the visible native positive");
    }
}
#[tokio::test]
async fn final_source_change_and_terminal_failures_withhold_the_frame_without_optional_followup() {
    for final_read in [
        Ok(vec![metadata("2020-01-01T06:00:00.200000Z", "2020-01-01T05:01:00Z", true)]),
        Err(CollectionStop::Cancelled.error()),
        Err(CollectionStop::CancellationUnconfirmed.error()),
    ] {
        let mut reader = reader(vec![actor("11", "active", Some("2020-01-01T05:30:00Z"))], true);
        reader.replies.pop_back();
        reader.replies.push_back(final_read);
        assert!(capture(&mut reader, "fixture.invalid", 5432, &request(500)).await.is_err());
        assert_eq!(reader.calls.len(), 3);
        assert!(reader.replies.is_empty());
    }
}
