use super::*;
use crate::{read::ReadResult, value::Val};
use std::collections::VecDeque;

struct Call {
    sql: String,
    parameters: Vec<Parameter>,
    limit: usize,
}
struct FakeRead {
    replies: VecDeque<ReadResult<Vec<Rec>>>,
    calls: Vec<Call>,
    closed: u32,
    shutdown: SourceShutdown,
}
#[async_trait::async_trait]
impl NativeRead for FakeRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.calls.push(Call { sql: sql.to_owned(), parameters, limit: row_limit });
        self.replies.pop_front().expect("unexpected native read")
    }
    fn queries(&self) -> u32 {
        self.calls.len() as u32
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed += 1;
        self.shutdown
    }
}
fn reader(
    replies: Vec<ReadResult<Vec<Rec>>>,
    scope: ActivityScope,
    identity: IdentityDetail,
) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead { replies: replies.into(), calls: Vec::new(), closed: 0, shutdown: SourceShutdown::Quiescent },
        ReadOptions { budget: CollectionBudget::default(), scope, identity },
        "fixture.invalid".into(),
        1433,
    )
}
fn replace(row: &Rec, values: &[(&str, Val)]) -> Rec {
    Rec::of(
        &row.columns()
            .map(|(name, value)| {
                (name, values.iter().find(|(key, _)| *key == name).map(|(_, value)| value).unwrap_or(value).clone())
            })
            .collect::<Vec<_>>(),
    )
}
fn metadata(id: i64) -> Rec {
    Rec::of(&[
        ("edition", Val::Int(5)),
        ("major", Val::Int(16)),
        ("server_version", Val::Text("16.0.1234.5".into())),
        ("compatibility_level", Val::Int(150)),
        ("reader_login", Val::Text("monitor".into())),
        ("reader_user", Val::Text("monitor".into())),
        ("database_wide", Val::Int(1)),
        ("database_name", Val::Text("shop".into())),
        ("incarnation", Val::Text("logical-guid".into())),
        ("physical_incarnation", Val::Text("physical-guid".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("native_observed_at", Val::Text("2020-01-01T08:00:00".into())),
        ("session_id", Val::Int(id)),
        ("session_generation", Val::Text("2020-01-01T07:00:00.1533333".into())),
        ("request_id", Val::Int(0)),
        ("request_generation", Val::Text("2020-01-01T07:58:00.3066667".into())),
        ("no_current_request", Val::Int(0)),
        ("scope_basis", Val::Int(1)),
        ("session_database", Val::Text("shop".into())),
        ("command", Val::Text("SELECT".into())),
        ("state", Val::Text("suspended".into())),
        ("program_name", Val::Text("app".into())),
        ("login_name", Val::Text("app_user".into())),
        ("host_name", Val::Text("app_host".into())),
        ("open_transaction_count", Val::Int(1)),
        ("transaction_id", Val::Int(123)),
        ("transaction_start", Val::Text("2020-01-01T07:58:30".into())),
        ("elapsed_ms", Val::Int(100_000)),
        ("last_request_start", Val::Text("2020-01-01T07:55:00".into())),
        ("last_request_end", Val::Text("2020-01-01T07:59:00".into())),
        ("session_cpu_ms", Val::Int(1000)),
        ("session_logical_reads", Val::Int(9007199254740993)),
        ("session_physical_reads", Val::Int(100)),
        ("session_writes", Val::Int(200)),
        ("request_cpu_ms", Val::Int(10)),
        ("request_logical_reads", Val::Int(20)),
        ("request_physical_reads", Val::Int(30)),
        ("request_writes", Val::Int(40)),
        ("wait_type", Val::Text("LCK_M_X".into())),
        ("wait_resource", Val::Null),
        ("wait_ms", Val::Int(40_000)),
        ("blocking_session_id", Val::Int(55)),
        ("blocker_generation", Val::Text("2020-01-01T07:00:00.1533333".into())),
        ("query_hash", Val::Text("0123456789ABCDEF".into())),
        ("sql_handle", Val::Text("AB".repeat(64))),
        ("statement_start", Val::Int(0)),
        ("statement_end", Val::Int(-1)),
        ("request_database_id", Val::Int(7)),
    ])
}

#[tokio::test]
async fn native_replica_facts_remain_readable_but_background_activity_requires_the_primary() {
    let readonly = replace(&metadata(42), &[("updateability", Val::Text("READ_ONLY".into()))]);
    let context = ActivityContext::decode(&readonly, "fixture.invalid", 1433, IdentityDetail::Endpoint).unwrap();
    assert_eq!(context.writable, Some(false));
    let mut reader = reader(vec![Ok(vec![readonly])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
    let error = crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(reader.query_count(), 1);
}
fn healthy(id: i64) -> Rec {
    replace(
        &metadata(id),
        &[
            ("elapsed_ms", Val::Int(100)),
            ("wait_type", Val::Null),
            ("wait_ms", Val::Null),
            ("blocking_session_id", Val::Int(0)),
            ("blocker_generation", Val::Null),
        ],
    )
}
fn idle(id: i64) -> Rec {
    replace(
        &metadata(id),
        &[
            ("request_id", Val::Null),
            ("request_generation", Val::Null),
            ("no_current_request", Val::Int(1)),
            ("scope_basis", Val::Int(2)),
            ("command", Val::Null),
            ("state", Val::Text("sleeping".into())),
            ("elapsed_ms", Val::Null),
            ("request_cpu_ms", Val::Null),
            ("request_logical_reads", Val::Null),
            ("request_physical_reads", Val::Null),
            ("request_writes", Val::Null),
            ("wait_type", Val::Null),
            ("wait_ms", Val::Null),
            ("blocking_session_id", Val::Int(0)),
            ("blocker_generation", Val::Null),
            ("query_hash", Val::Null),
            ("sql_handle", Val::Null),
            ("statement_start", Val::Null),
            ("statement_end", Val::Null),
            ("request_database_id", Val::Null),
        ],
    )
}
fn details(id: i64, sql: &str) -> Rec {
    Rec::of(&[
        ("session_id", Val::Int(id)),
        ("session_generation", Val::Text("2020-01-01T07:00:00.1533333".into())),
        ("request_id", Val::Int(0)),
        ("request_generation", Val::Text("2020-01-01T07:58:00.3066667".into())),
        ("matched", Val::Int(1)),
        ("encrypted", Val::Bool(false)),
        ("source_bytes", Val::Int((sql.encode_utf16().count() * 2) as i64)),
        ("payload", Val::Text(sql.into())),
    ])
}
fn xml_parameter(call: &Call) -> &str {
    match &call.parameters[0] {
        Parameter::Text(value) => value,
        _ => panic!("expected typed selectors"),
    }
}

#[tokio::test]
async fn live_and_export_share_request_facts_with_different_selection_and_payload_policies() {
    let sql = "SELECT N'private-activity-canary' FROM dbo.orders";
    let rows = vec![metadata(42), healthy(99)];
    let mut interactive = reader(
        vec![Ok(rows.clone()), Ok(vec![details(99, "SELECT 1"), details(42, sql)])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut interactive).await.unwrap();
    assert_eq!(live.sessions.len(), 2);
    let session = live.sessions.iter().find(|session| session.id == "42").unwrap();
    assert_eq!(session.cpu_ms, Some(10.0));
    assert_eq!(session.reads, Some(20.0));
    assert_eq!(session.transaction_ms, Some(90_000.0));
    assert_eq!(session.database.as_deref(), Some("shop"));
    assert_eq!(session.command.as_deref(), Some("SELECT"));
    assert_eq!(session.sql.as_deref(), Some(sql));
    assert_eq!(interactive.io.closed, 1);
    assert!(interactive.io.replies.is_empty());
    let mut exporting = reader(
        vec![Ok(rows), Ok(vec![details(42, sql)])],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (description, observations, blocking) =
        crate::telemetry::activity::capture(&mut exporting, &ActivityPolicy::default()).await.unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(blocking.len(), 1);
    let row = &observations[0];
    assert_eq!(row.key.session.id, session.id);
    assert_eq!(row.request_counters.as_ref().unwrap().cpu_ms.as_deref(), Some("10"));
    assert_eq!(row.session_counters.logical_reads.as_deref(), Some("9007199254740993"));
    assert_eq!(row.database, session.database);
    assert_eq!(row.command, session.command);
    assert_eq!(row.transaction_age_ms.as_deref(), Some("90000"));
    assert!(!serde_json::to_string(row).unwrap().contains("private-activity-canary"));
    assert_eq!(description.observation.activity().unwrap().native_rows, 2);
    assert_eq!(exporting.query_count(), 2);
    assert_eq!(interactive.query_count(), 2);
    assert!(xml_parameter(&interactive.io.calls[1]).contains("session=\"99\""));
    assert!(!xml_parameter(&exporting.io.calls[1]).contains("session=\"99\""));
}
#[tokio::test]
async fn idle_previous_batches_remain_visible_without_becoming_current_export_statements() {
    let last = "UPDATE dbo.orders SET status=N'previous-batch-canary'";
    let rows = vec![metadata(42), idle(55)];
    let mut interactive = reader(
        vec![Ok(rows.clone()), Ok(vec![details(42, "SELECT 1")]), Ok(vec![details(55, last)])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut interactive).await.unwrap();
    let sleeper = live.sessions.iter().find(|session| session.id == "55").unwrap();
    assert_eq!(sleeper.sql.as_deref(), Some(last));
    assert_eq!(sleeper.elapsed_ms, Some(300_000.0));
    assert_eq!(sleeper.cpu_ms, Some(1000.0));
    assert_eq!(interactive.io.calls[2].sql, sql::PREVIOUS_BATCH);
    let mut exporting = reader(
        vec![Ok(rows), Ok(vec![details(42, "SELECT 1")])],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (_, observations, _) =
        crate::telemetry::activity::capture(&mut exporting, &ActivityPolicy::default()).await.unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(exporting.query_count(), 2);
    let sleeper = observations.iter().find(|row| row.key.session.id == "55").unwrap();
    assert_eq!(sleeper.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    assert!(!serde_json::to_string(&observations).unwrap().contains("previous-batch-canary"));
}
#[tokio::test]
async fn quiet_and_healthy_export_polls_do_not_fetch_sql() {
    for row in [replace(&metadata(42), &[("session_id", Val::Null)]), healthy(42)] {
        let mut reader = reader(vec![Ok(vec![row])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
        let (_, observations, blocking) =
            crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.unwrap();
        assert!(observations.is_empty());
        assert!(blocking.is_empty());
        assert_eq!(reader.query_count(), 1);
    }
}
#[tokio::test]
async fn native_identity_and_age_keep_local_ticks_without_treating_server_time_as_utc() {
    let mut reader = reader(vec![Ok(vec![metadata(42)])], ActivityScope::Visible, IdentityDetail::Endpoint);
    let frame = reader.observe().await.unwrap();
    let row = &frame.records[0].facts;
    assert_eq!(frame.context.at, "2020-01-01T06:00:00.000000000Z");
    assert_eq!(row.key.session.generation, "2020-01-01T07:00:00.1533333");
    assert_eq!(row.key.request.as_ref().unwrap().generation, "2020-01-01T07:58:00.3066667");
    assert_eq!(row.transaction_age_ms.as_deref(), Some("90000"));
    assert_eq!(row.session_counters.logical_reads.as_deref(), Some("9007199254740993"));
    for start in ["2020-01-01T08:00:01", "2020-01-01T08:00:00.0000001"] {
        let input = replace(&metadata(42), &[("transaction_start", Val::Text(start.into()))]);
        assert!(rows::record(&input).unwrap().unwrap().facts.transaction_age_ms.is_none());
    }
}
#[tokio::test]
async fn a_reused_request_or_advancing_statement_never_supplies_unowned_text() {
    let response = replace(&details(42, "SELECT N'wrong-statement-canary'"), &[("matched", Val::Int(0))]);
    let mut reader = reader(
        vec![Ok(vec![metadata(42)]), Ok(vec![response])],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (_, observations, _) =
        crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.unwrap();
    assert_eq!(observations[0].statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing });
}
#[tokio::test]
async fn sql_handles_and_offsets_allow_statements_without_query_hashes() {
    for hash in [Val::Null, Val::Text("0000000000000000".into())] {
        let metadata = replace(&metadata(42), &[("query_hash", hash), ("command", Val::Text("WAITFOR".into()))]);
        let mut reader = reader(
            vec![Ok(vec![metadata]), Ok(vec![details(42, "WAITFOR DELAY '00:00:10'")])],
            ActivityScope::Visible,
            IdentityDetail::Endpoint,
        );
        let live = crate::live::collect_from(&mut reader).await.unwrap();
        assert_eq!(live.sessions[0].sql.as_deref(), Some("WAITFOR DELAY '00:00:10'"));
        let selectors = xml_parameter(&reader.io.calls[1]);
        assert!(selectors.contains("handle=\"ABAB"));
        assert!(selectors.contains("begin=\"0\" end=\"-1\""));
    }
}
#[tokio::test]
async fn multiple_requests_on_one_session_keep_their_own_text_even_when_results_are_reordered() {
    let second = replace(
        &metadata(42),
        &[("request_id", Val::Int(1)), ("request_generation", Val::Text("2020-01-01T07:59:00".into()))],
    );
    let second_detail = replace(
        &details(42, "SELECT * FROM dbo.second_request"),
        &[("request_id", Val::Int(1)), ("request_generation", Val::Text("2020-01-01T07:59:00".into()))],
    );
    let mut reader = reader(
        vec![Ok(vec![metadata(42), second]), Ok(vec![second_detail, details(42, "SELECT * FROM dbo.first_request")])],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (_, observations, _) =
        crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.unwrap();
    assert_eq!(observations.len(), 2);
    let text = serde_json::to_string(&observations).unwrap();
    assert!(text.contains("first_request") && text.contains("second_request"));
    assert_eq!(observations[0].key.request.as_ref().unwrap().id, "0");
}
#[tokio::test]
async fn null_request_counters_never_fall_back_to_session_lifetime_totals() {
    let input = replace(&metadata(42), &[("request_cpu_ms", Val::Null), ("request_logical_reads", Val::Int(0))]);
    let mut reader = reader(
        vec![Ok(vec![input]), Ok(vec![details(42, "SELECT 1")])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut reader).await.unwrap();
    assert_eq!(live.sessions[0].cpu_ms, None);
    assert_eq!(live.sessions[0].reads, Some(0.0));
}
#[tokio::test]
async fn restricted_or_overflowed_activity_never_looks_like_a_clean_empty_database() {
    let restricted = replace(&metadata(42), &[("database_wide", Val::Int(0))]);
    let mut reader = reader(vec![Ok(vec![restricted])], ActivityScope::Visible, IdentityDetail::Endpoint);
    let error = crate::live::collect_from(&mut reader).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(error.message.contains("GRANT VIEW DATABASE STATE"));
    assert_eq!(reader.io.closed, 1);
    let mut reader = super::tests::reader(
        vec![Ok(vec![metadata(42), metadata(43)])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    reader.options.budget.max_snapshot_rows = 1;
    let error = crate::live::collect_from(&mut reader).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.io.calls[0].limit, 2);
}
#[tokio::test]
async fn optional_detail_failure_retains_metadata_with_explicit_unavailability() {
    let mut reader = reader(
        vec![Ok(vec![metadata(42)]), Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "fixture")))],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut reader).await.unwrap();
    assert_eq!(live.sessions.len(), 1);
    assert_eq!(live.sessions[0].reads, Some(20.0));
    assert!(live.sessions[0].sql.is_none());
    assert_eq!(live.notes.len(), 1);
    assert_eq!(reader.io.closed, 1);
}
#[tokio::test]
async fn stopped_or_unconfirmed_reads_close_owned_work_and_never_return_fresh_live_counts() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::ByteLimit] {
        let mut reader = reader(
            vec![Ok(vec![metadata(42)]), Err(ReadError::Stopped(stop))],
            ActivityScope::Visible,
            IdentityDetail::Endpoint,
        );
        let error = crate::live::collect_from(&mut reader).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.io.closed, 1);
    }
    let mut reader = reader(
        vec![Ok(vec![replace(&metadata(42), &[("session_id", Val::Null)])])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    reader.io.shutdown = SourceShutdown::Unconfirmed;
    let error = crate::live::collect_from(&mut reader).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
}
#[tokio::test]
async fn other_databases_and_unknown_wait_owners_do_not_become_database_incidents() {
    let other =
        replace(&metadata(42), &[("scope_basis", Val::Int(5)), ("session_database", Val::Text("elsewhere".into()))]);
    let unknown = replace(
        &healthy(43),
        &[
            ("wait_type", Val::Text("ASYNC_DISKPOOL_LOCK".into())),
            ("wait_ms", Val::Int(40000)),
            ("blocking_session_id", Val::Int(-5)),
        ],
    );
    let mut reader =
        reader(vec![Ok(vec![other, unknown])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
    let (_, observations, blocking) =
        crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.unwrap();
    assert!(observations.is_empty());
    assert!(blocking.is_empty());
    for id in [-2, -3, -4, -5, -6] {
        let row = replace(&metadata(42), &[("blocking_session_id", Val::Int(id))]);
        let owner = rows::record(&row).unwrap().unwrap().facts.wait.unwrap().owners.remove(0);
        assert_eq!(owner.is_blocker(), id == -2 || id == -3);
    }
}
#[tokio::test]
async fn malformed_detail_sets_and_oversized_sql_do_not_replace_valid_metadata() {
    for (payload, expected_details, expected_reason) in [
        (
            replace(&details(42, "SELECT 1"), &[("source_bytes", Val::Int(999999))]),
            ActivityDetailStatus::Collected,
            PayloadUnavailable::Oversized,
        ),
        (
            replace(&details(99, "SELECT 1"), &[("matched", Val::Int(1))]),
            ActivityDetailStatus::Unavailable { code: ErrorCode::QueryFailed },
            PayloadUnavailable::SourceUnavailable,
        ),
    ] {
        let mut reader = reader(
            vec![Ok(vec![metadata(42)]), Ok(vec![payload])],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let (description, observations, _) =
            crate::telemetry::activity::capture(&mut reader, &ActivityPolicy::default()).await.unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].request_counters.as_ref().unwrap().cpu_ms.as_deref(), Some("10"));
        assert_eq!(observations[0].statement, DefinitionPayload::Unavailable { reason: expected_reason });
        assert_eq!(description.observation.activity().unwrap().details, expected_details);
    }
}
#[tokio::test]
async fn duplicate_or_reused_native_identities_in_one_read_are_rejected() {
    for duplicate in
        [metadata(42), replace(&metadata(42), &[("session_generation", Val::Text("2020-01-01T07:01:00".into()))])]
    {
        let mut reader =
            reader(vec![Ok(vec![metadata(42), duplicate])], ActivityScope::Visible, IdentityDetail::Endpoint);
        assert!(crate::live::collect_from(&mut reader).await.is_err());
        assert_eq!(reader.io.closed, 1);
    }
}
#[tokio::test]
async fn endpoint_projection_works_for_sql_server_without_azure_database_identity() {
    let row = replace(
        &metadata(42),
        &[
            ("edition", Val::Int(3)),
            ("major", Val::Int(12)),
            ("incarnation", Val::Null),
            ("physical_incarnation", Val::Null),
        ],
    );
    let mut reader = reader(
        vec![Ok(vec![row]), Ok(vec![details(42, "SELECT 1")])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut reader).await.unwrap();
    assert_eq!(live.sessions.len(), 1);
    assert_eq!(reader.io.calls[0].sql, sql::METADATA);
    assert_eq!(reader.query_count(), 2);
}

#[tokio::test]
async fn source_representation_changes_start_a_new_activity_epoch() {
    let mut original =
        reader(vec![Ok(vec![metadata(42)])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
    let original = original.observe().await.unwrap();
    let changed = replace(&metadata(42), &[("compatibility_level", Val::Int(120))]);
    let mut changed = reader(vec![Ok(vec![changed])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
    let changed = changed.observe().await.unwrap();
    assert_ne!(original.context.epoch, changed.context.epoch);
}

#[test]
fn native_sql_handles_preserve_variable_length_binary_values() {
    for length in [1, 44, 64] {
        let handle = "AB".repeat(length);
        let row = replace(&metadata(42), &[("sql_handle", Val::Text(handle.clone()))]);
        assert_eq!(rows::record(&row).unwrap().unwrap().current_locator.unwrap().handle, handle);
    }
    for handle in [String::new(), "A".into(), "AZ".into(), "AB".repeat(65)] {
        let row = replace(&metadata(42), &[("sql_handle", Val::Text(handle))]);
        assert!(rows::record(&row).is_err());
    }
}

#[tokio::test]
async fn native_requests_without_a_statement_span_preserve_counters_without_fetching_sql() {
    for start in [Val::Int(-1), Val::Null] {
        let metadata = replace(
            &metadata(42),
            &[("statement_start", start), ("statement_end", Val::Int(-1)), ("command", Val::Text("EXECUTE".into()))],
        );
        let mut interactive =
            reader(vec![Ok(vec![metadata.clone()])], ActivityScope::Visible, IdentityDetail::Endpoint);
        let live = crate::live::collect_from(&mut interactive).await.unwrap();
        assert_eq!(live.sessions.len(), 1);
        assert_eq!(live.sessions[0].cpu_ms, Some(10.0));
        assert!(live.sessions[0].sql.is_none());
        assert_eq!(interactive.query_count(), 1);
        let mut exporting =
            reader(vec![Ok(vec![metadata])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
        let (_, observations, _) =
            crate::telemetry::activity::capture(&mut exporting, &ActivityPolicy::default()).await.unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].request_counters.as_ref().unwrap().cpu_ms.as_deref(), Some("10"));
        assert_eq!(observations[0].statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing });
        assert_eq!(exporting.query_count(), 1);
    }
}

#[tokio::test]
async fn an_observed_request_wins_over_a_later_absence_probe_without_attributing_ended_sql() {
    let metadata = replace(&metadata(42), &[("no_current_request", Val::Int(1))]);
    let ended = replace(&details(42, "SELECT N'ended-statement-canary'"), &[("matched", Val::Int(0))]);
    let mut interactive = reader(
        vec![Ok(vec![metadata.clone()]), Ok(vec![ended.clone()])],
        ActivityScope::Visible,
        IdentityDetail::Endpoint,
    );
    let live = crate::live::collect_from(&mut interactive).await.unwrap();
    assert_eq!(live.sessions.len(), 1);
    assert_eq!(live.sessions[0].cpu_ms, Some(10.0));
    assert_eq!(live.sessions[0].command.as_deref(), Some("SELECT"));
    assert!(live.sessions[0].sql.is_none());
    let mut exporting = reader(
        vec![Ok(vec![metadata]), Ok(vec![ended])],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (_, observations, _) =
        crate::telemetry::activity::capture(&mut exporting, &ActivityPolicy::default()).await.unwrap();
    assert_eq!(observations.len(), 1);
    assert!(!observations[0].idle);
    assert!(observations[0].key.request.is_some());
    assert_eq!(observations[0].request_counters.as_ref().unwrap().cpu_ms.as_deref(), Some("10"));
    assert_eq!(observations[0].statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing });
}

#[tokio::test]
async fn page_wait_targets_share_the_request_identity_without_inventing_a_holder_or_more_reads() {
    for (wait, kind) in [
        ("PAGELATCH_EX", culprit_core::WaitResourceKind::MemoryPage),
        ("PAGEIOLATCH_SH", culprit_core::WaitResourceKind::PageIo),
    ] {
        let row = replace(
            &metadata(42),
            &[
                ("wait_type", Val::Text(wait.into())),
                ("wait_resource", Val::Text("5:1:4144".into())),
                ("blocking_session_id", Val::Int(-5)),
                ("blocker_generation", Val::Null),
            ],
        );
        let sql = "SELECT id FROM dbo.orders";
        let mut interactive = reader(
            vec![Ok(vec![row.clone()]), Ok(vec![details(42, sql)])],
            ActivityScope::Visible,
            IdentityDetail::Endpoint,
        );
        let live = crate::live::collect_from(&mut interactive).await.unwrap();
        let session = &live.sessions[0];
        let targets = session.wait_resources.as_ref().unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, kind);
        assert_eq!(targets[0].native.id, "5:1:4144");
        assert_eq!(targets[0].native.qualifiers["source"], "sys.dm_exec_requests.wait_resource");
        assert!(session.blocked_by.is_none(), "a waited-on page is not its holder");
        assert_eq!(session.sql.as_deref(), Some(sql));
        let mut exporting = reader(
            vec![Ok(vec![row]), Ok(vec![details(42, sql)])],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let (_, observations, blocking) =
            crate::telemetry::activity::capture(&mut exporting, &ActivityPolicy::default()).await.unwrap();
        let wait = observations[0].wait.as_ref().unwrap();
        assert_eq!(&wait.identified_resources, targets);
        assert!(wait.owners.iter().all(|owner| !owner.is_blocker()));
        assert_eq!(observations[0].conditions(&ActivityPolicy::default()), vec![ActivityCondition::LongRunning]);
        assert!(blocking.is_empty());
        assert_eq!(interactive.query_count(), 2);
        assert_eq!(exporting.query_count(), 2);
    }
}

#[tokio::test]
async fn an_unidentified_page_stays_unknown_and_invalid_native_identifiers_cannot_complete_capture() {
    let row = replace(&metadata(42), &[("wait_type", Val::Text("PAGELATCH_EX".into())), ("wait_resource", Val::Null)]);
    let mut unknown = reader(vec![Ok(vec![row])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
    let frame = unknown.observe().await.unwrap();
    assert!(frame.records[0].facts.wait.as_ref().unwrap().identified_resources.is_empty());
    for invalid in ["control\nidentifier".to_owned(), "x".repeat(513)] {
        let row = replace(
            &metadata(42),
            &[("wait_type", Val::Text("PAGELATCH_EX".into())), ("wait_resource", Val::Text(invalid))],
        );
        let mut reader = reader(vec![Ok(vec![row])], ActivityScope::CurrentDatabase, IdentityDetail::AzureDatabase);
        assert!(reader.observe().await.is_err());
        assert_eq!(reader.query_count(), 1);
    }
}
