use super::*;
use std::collections::VecDeque;

const AT: &str = "2020-01-01T06:00:00Z";
const STARTED: &str = "2020-01-01T05:00:00Z";
const QUERY: &str = "2020-01-01T05:59:00Z";

struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<(String, usize)>,
}
impl Reader {
    fn new(replies: Vec<Result<Vec<Row>>>) -> Self {
        Self { replies: replies.into(), calls: Vec::new() }
    }
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls.push((sql.into(), limit));
        self.replies.pop_front().expect("only the planned bounded native reads")
    }
}
fn metadata(pid: u32, database: &str, waiting: bool) -> Row {
    Row::from_fields(
        [
            ("observed_at", Some(AT.to_owned())),
            ("pid", Some(pid.to_string())),
            ("backend_start", Some(STARTED.to_owned())),
            ("query_start", Some(QUERY.to_owned())),
            ("transaction_start", Some(QUERY.to_owned())),
            ("database_oid", Some(database.to_owned())),
            ("database", Some(if database == "42" { "shop" } else { "other" }.into())),
            ("state", Some("active".to_owned())),
            ("wait_event_type", waiting.then(|| "Lock".to_owned())),
            ("wait_event", waiting.then(|| "transactionid".to_owned())),
            ("elapsed_ms", Some("60000".to_owned())),
            ("transaction_ms", Some("60000".to_owned())),
            ("idle_ms", Some("0".to_owned())),
            ("query_id", Some("7".to_owned())),
            ("application_name", Some("fixture".to_owned())),
        ]
        .map(|(name, value)| (name.into(), value)),
    )
}
fn changed(row: Row, name: &str, value: &str) -> Row {
    with_value(row, name, Some(value))
}
fn with_value(row: Row, name: &str, value: Option<&str>) -> Row {
    const FIELDS: &[&str] = &[
        "observed_at",
        "pid",
        "backend_start",
        "query_start",
        "transaction_start",
        "database_oid",
        "database",
        "state",
        "wait_event_type",
        "wait_event",
        "elapsed_ms",
        "transaction_ms",
        "idle_ms",
        "query_id",
        "application_name",
        "blocking_waiters",
    ];
    assert!(FIELDS.contains(&name));
    Row::from_fields(
        FIELDS
            .iter()
            .map(|field| ((*field).into(), if *field == name { value.map(str::to_owned) } else { row.text(field) })),
    )
}
fn context(row: Row, waiters: &[u32]) -> Row {
    changed(row, "blocking_waiters", &serde_json::to_string(waiters).unwrap())
}
fn edges(pid: u32, owners: &[u32]) -> Row {
    Row::from_fields([
        ("pid".into(), Some(pid.to_string())),
        ("owners".into(), Some(serde_json::to_string(owners).unwrap())),
    ])
}
fn absent() -> Row {
    Row::from_fields([("observed_at".into(), Some(AT.into())), ("pid".into(), None)])
}
fn lock(pid: u32) -> Row {
    Row::from_fields([
        ("pid".into(), Some(pid.to_string())),
        ("elapsed_ms".into(), Some("40000".into())),
        ("locktype".into(), Some("transactionid".into())),
        ("mode".into(), Some("ShareLock".into())),
    ])
}
fn activity_frame(rows: Vec<Row>) -> ActivityFrame {
    ActivityFrame {
        at: AT.into(),
        records: rows.into_iter().map(|row| decode(row, "42").unwrap()).collect(),
        locks: Vec::new(),
    }
}
fn options() -> BlockingReadOptions<'static> {
    BlockingReadOptions {
        minimum_request_age_ms: 30_000,
        scope: ActivityScope::CurrentDatabase,
        version: 180_000,
        database_oid: "42",
        max_snapshot_rows: 500,
    }
}
fn owners(frame: &ActivityFrame, index: usize) -> &[ActivityWaitOwner] {
    &frame.records[index].facts.as_ref().unwrap().wait.as_ref().unwrap().owners
}
fn assert_unresolved(frame: &ActivityFrame, index: usize, pid: u32) {
    assert_eq!(owners(frame, index), &[ActivityWaitOwner::Session { id: pid.to_string(), generation: None }]);
}

#[test]
fn native_root_database_filter_precedes_limit_for_postgres_and_shared_ysql_layouts() {
    for (query_id, backends, extra) in [
        (true, "'client backend'", ""),
        (false, "'client backend','yb-conn-mgr worker connection'", ",a.yb_backend_xid::text AS transaction_id"),
    ] {
        let sql = query_layout(query_id, backends, extra, 3);
        let selected_database = sql
            .find("AND datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database())")
            .unwrap();
        assert!(selected_database < sql.find("LIMIT 3").unwrap());
        assert!(sql.contains(&format!("AND backend_type IN ({backends}) LIMIT 3")));
        assert!(sql.contains("a.backend_start") && sql.contains("a.query_start"));
        assert!(!sql.contains("a.query,"), "the baseline metadata read collects no statement text");
    }
}

#[tokio::test]
async fn healthy_or_young_database_requests_add_no_blocker_context_or_lock_reads() {
    let young = changed(metadata(11, "42", true), "elapsed_ms", "29999");
    let mut frame = activity_frame(vec![metadata(10, "42", false), young]);
    let mut reader = Reader::new(vec![]);
    blockers(&mut reader, &mut frame, options()).await.unwrap();
    assert!(reader.calls.is_empty());
    assert!(frame.locks.is_empty());
}

#[tokio::test]
async fn database_root_row_witness_remains_a_stop_before_any_blocker_read() {
    let mut reader = Reader::new(vec![Ok(vec![metadata(10, "42", false), metadata(11, "42", false)])]);
    let error = observe_limited(&mut reader, &query(180_000, 2), "42", 1).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].1, 2);
}

#[tokio::test]
async fn only_reached_cross_database_blockers_join_the_same_native_frame_and_guarded_sql_selection() {
    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let idle = changed(metadata(30, "44", false), "state", "idle in transaction");
    let mut reader = Reader::new(vec![
        Ok(vec![edges(10, &[20])]),
        Ok(vec![context(metadata(20, "43", true), &[10])]),
        Ok(vec![edges(20, &[30])]),
        Ok(vec![context(idle, &[20])]),
        Ok(vec![lock(10), lock(20)]),
    ]);
    blockers(&mut reader, &mut frame, options()).await.unwrap();
    assert_eq!(reader.calls.len(), 5);
    assert_eq!(frame.records.len(), 3);
    assert_eq!(frame.records[0].facts.as_ref().unwrap().scope, ActivityScopeBasis::SessionDatabase);
    assert!(
        frame.records[1..].iter().all(|row| row.facts.as_ref().unwrap().scope == ActivityScopeBasis::OtherDatabase)
    );
    assert!(owners(&frame, 0)[0].session().is_some_and(|session| session.id == "20" && session.generation == STARTED));
    assert!(owners(&frame, 1)[0].session().is_some_and(|session| session.id == "30" && session.generation == STARTED));
    let selected = ActivityEvidence::select(&frame.observations(), &ActivityPolicy::default()).unwrap();
    assert_eq!(selected.observations.len(), 3);
    assert_eq!(selected.blocking.len(), 2);
    assert!(selected.blocking.iter().all(|edge| edge.owner_observed));
    for call in [&reader.calls[1], &reader.calls[3]] {
        assert!(call.0.contains("native_owners AS MATERIALIZED"));
        assert!(
            call.0.contains("a.backend_start=t.backend_start")
                && call.0.contains("a.query_start IS NOT DISTINCT FROM t.query_start")
        );
        assert!(call.0.contains("WHERE a.wait_event_type='Lock'"));
        assert!(!call.0.contains("AND datid="), "only reached owner IDs can cross the selected database boundary");
        assert_eq!(call.1, 2);
    }
    assert!(reader.calls[1].0.contains("WHERE owner_pid IN (20)"));
    assert!(reader.calls[3].0.contains("WHERE owner_pid IN (30)"));
    assert!(
        reader.calls[4].0.contains("w.database=0 OR w.database="),
        "foreign relation IDs cannot borrow local catalog names"
    );
    let keys = selected.observations.iter().map(|row| row.key.clone()).collect();
    reader.replies.push_back(Ok(vec![]));
    enrich(&mut reader, &mut frame, &keys, 1024, true).await.unwrap();
    let sql = &reader.calls[5].0;
    assert!(
        sql.contains("a.backend_start=t.backend_start")
            && sql.contains("a.query_start IS NOT DISTINCT FROM t.query_start")
    );
    assert!(
        frame.records.iter().all(|row| row.sql.is_none()),
        "a disappeared statement cannot inherit another backend's SQL"
    );
}

#[tokio::test]
async fn missing_recycled_or_replaced_owner_context_never_inherits_the_original_statement() {
    for observed in [
        absent(),
        context(changed(metadata(20, "42", false), "backend_start", "2020-01-01T05:30:00Z"), &[10]),
        context(changed(metadata(20, "42", false), "query_start", "2020-01-01T05:59:59Z"), &[10]),
    ] {
        let mut frame = activity_frame(vec![metadata(10, "42", true), metadata(20, "42", false)]);
        frame.records[1].sql = Some("DELETE FROM private_old_request".into());
        let original_key = frame.records[1].facts.as_ref().unwrap().key.clone();
        let mut reader = Reader::new(vec![Ok(vec![edges(10, &[20])]), Ok(vec![observed]), Ok(vec![lock(10)])]);
        blockers(&mut reader, &mut frame, options()).await.unwrap();
        assert_eq!(frame.records.len(), 2);
        assert_eq!(frame.records[1].facts.as_ref().unwrap().key, original_key);
        assert_unresolved(&frame, 0, 20);
        assert_eq!(reader.calls.len(), 3);
        let sessions = frame.records.iter().map(project).collect::<Vec<_>>();
        assert!(
            sessions[1].sql.as_ref().is_some_and(|sql| sql.contains("private_old_request")),
            "preserve the original observed session row"
        );
        let blocking =
            culprit_core::blocking::analyze(Engine::Postgres, &sessions, &frame.locks, &unqualified_owner_ids(&frame))
                .unwrap();
        assert_eq!(blocking.waiting, 1);
        assert_eq!(blocking.chains[0].head, "20");
        assert_eq!(blocking.chains[0].head_kind, culprit_core::HeadKind::Unknown);
        assert!(blocking.chains[0].head_ms.is_none());
        assert!(!blocking.chains[0].headline.contains("DELETE"));
        assert_eq!(
            frame.records[0].facts.as_ref().unwrap().wait.as_ref().unwrap().native_type.as_deref(),
            Some("Lock: transactionid")
        );
    }
}

#[tokio::test]
async fn continued_native_edge_qualifies_only_its_actual_waiter_and_shared_owner_is_not_duplicated() {
    for waiters in [vec![11], vec![10, 11]] {
        let mut frame = activity_frame(vec![metadata(10, "42", true), metadata(11, "42", true)]);
        let mut reader = Reader::new(vec![
            Ok(vec![edges(10, &[20]), edges(11, &[20])]),
            Ok(vec![context(metadata(20, "43", false), &waiters)]),
            Ok(vec![lock(10), lock(11)]),
        ]);
        blockers(&mut reader, &mut frame, options()).await.unwrap();
        assert_eq!(frame.records.len(), 3);
        if waiters.len() == 1 {
            assert_unresolved(&frame, 0, 20);
        } else {
            assert!(owners(&frame, 0)[0].session().is_some());
        }
        assert!(owners(&frame, 1)[0].session().is_some());
        assert_eq!(reader.calls.len(), 3);
        assert_eq!(reader.calls[1].1, 2, "one unique owner plus the row witness");
    }
}

#[tokio::test]
async fn masked_foreign_owner_context_remains_unresolved_and_cannot_supply_sql() {
    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let masked = with_value(metadata(20, "43", false), "backend_start", None);
    let mut reader =
        Reader::new(vec![Ok(vec![edges(10, &[20])]), Ok(vec![context(masked, &[10])]), Ok(vec![lock(10)])]);
    blockers(&mut reader, &mut frame, options()).await.unwrap();
    assert_eq!(frame.records.len(), 1);
    assert_unresolved(&frame, 0, 20);
    let selected = ActivityEvidence::select(&frame.observations(), &ActivityPolicy::default()).unwrap();
    assert_eq!(selected.blocking.len(), 1);
    assert!(!selected.blocking[0].owner_observed);
    assert_eq!(reader.calls.len(), 3);
}

#[tokio::test]
async fn denied_context_and_unrequested_edges_cannot_become_a_complete_quiet_read() {
    let denied = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied");
    for (replies, expected) in
        [(vec![Ok(vec![edges(10, &[20])]), Err(denied.clone())], denied), (vec![Ok(vec![edges(99, &[20])])], invalid())]
    {
        let count = replies.len();
        let mut frame = activity_frame(vec![metadata(10, "42", true)]);
        let mut reader = Reader::new(replies);
        let error = blockers(&mut reader, &mut frame, options()).await.unwrap_err();
        assert_eq!(error.code, expected.code);
        assert_eq!(error.detail, expected.detail);
        assert_eq!(reader.calls.len(), count);
        let original = frame.records[0].facts.as_ref().unwrap();
        assert_eq!(original.blocking_status, ActivityBlockingStatus::Unknown);
        assert_eq!(original.wait.as_ref().unwrap().native_type.as_deref(), Some("Lock: transactionid"));
        assert!(original.wait.as_ref().unwrap().resources.is_none());
    }
}

#[tokio::test]
async fn a_changed_or_ended_waiter_preserves_the_observed_wait_as_uncertain() {
    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let mut reader = Reader::new(vec![Ok(vec![]), Ok(vec![])]);
    blockers(&mut reader, &mut frame, options()).await.unwrap();
    assert_eq!(reader.calls.len(), 2, "no owner lookup when the waiter no longer matches");
    let facts = frame.records[0].facts.as_ref().unwrap();
    assert_eq!(facts.blocking_status, ActivityBlockingStatus::Unknown);
    assert_eq!(facts.wait.as_ref().unwrap().native_type.as_deref(), Some("Lock: transactionid"));
    let selected = ActivityEvidence::select(&frame.observations(), &ActivityPolicy::default()).unwrap();
    assert!(selected.uncertain.contains(&facts.key));
    assert_eq!(selected.observations.len(), 1);
}

#[tokio::test]
async fn invalid_context_or_regressed_clock_stops_without_further_native_work() {
    for (rows, expected) in [
        (vec![context(metadata(21, "43", false), &[10])], CollectionStop::InvalidPage),
        (vec![context(metadata(20, "43", false), &[99])], CollectionStop::InvalidPage),
        (
            vec![context(changed(metadata(20, "43", false), "observed_at", "2020-01-01T05:59:59Z"), &[10])],
            CollectionStop::SourceChanged,
        ),
        (
            vec![context(metadata(20, "43", false), &[10]), context(metadata(20, "43", false), &[10])],
            CollectionStop::RowLimit,
        ),
    ] {
        let mut frame = activity_frame(vec![metadata(10, "42", true)]);
        let mut reader = Reader::new(vec![Ok(vec![edges(10, &[20])]), Ok(rows)]);
        let error = blockers(&mut reader, &mut frame, options()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), expected);
        assert_eq!(reader.calls.len(), 2);
    }
}

#[tokio::test]
async fn cancellation_and_native_limits_do_not_issue_followup_or_claim_complete_empty_context() {
    for replies in [
        vec![Err(CollectionStop::TimedOut.error())],
        vec![Ok(vec![edges(10, &[20])]), Err(CollectionStop::TimedOut.error())],
    ] {
        let count = replies.len();
        let mut frame = activity_frame(vec![metadata(10, "42", true)]);
        let mut reader = Reader::new(replies);
        let error = blockers(&mut reader, &mut frame, options()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
        assert_eq!(reader.calls.len(), count);
    }
    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let mut reader = Reader::new(vec![Ok(vec![edges(10, &(1..=65).collect::<Vec<_>>())])]);
    let error = blockers(&mut reader, &mut frame, options()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.calls.len(), 1);

    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let mut reader = Reader::new(vec![Ok(vec![edges(10, &[20])]), Ok(vec![context(metadata(20, "43", false), &[10])])]);
    let mut options = options();
    options.max_snapshot_rows = 1;
    let error = blockers(&mut reader, &mut frame, options).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.calls.len(), 2);
    assert_eq!(frame.records.len(), 1);
}

#[tokio::test]
async fn closure_depth_is_bounded_and_prepared_owners_need_no_backend_lookup() {
    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let mut replies = Vec::new();
    for waiter in 10..18 {
        replies.push(Ok(vec![edges(waiter, &[waiter + 1])]));
        replies.push(Ok(vec![context(metadata(waiter + 1, "43", true), &[waiter])]));
    }
    let mut reader = Reader::new(replies);
    let error = blockers(&mut reader, &mut frame, options()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.calls.len(), 16);
    assert!(reader.calls.iter().all(|(sql, _)| !sql.contains("pg_catalog.pg_locks")));

    let mut frame = activity_frame(vec![metadata(10, "42", true)]);
    let mut reader = Reader::new(vec![Ok(vec![edges(10, &[0])]), Ok(vec![lock(10)])]);
    blockers(&mut reader, &mut frame, options()).await.unwrap();
    assert_eq!(
        owners(&frame, 0),
        &[ActivityWaitOwner::Transaction { native_id: "0".into(), owner: TransactionWaitOwner::Prepared }]
    );
    assert_eq!(reader.calls.len(), 2);
}
