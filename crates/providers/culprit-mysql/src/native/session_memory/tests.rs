use super::*;
use crate::{native::error_log::context::tests as instance_rows, read::test_support::Read};

pub(crate) fn consumer_rows(global: bool, thread: bool, statements: bool) -> Vec<Rec> {
    [("global_instrumentation", global), ("thread_instrumentation", thread), ("events_statements_current", statements)]
        .into_iter()
        .map(|(name, enabled)| {
            Rec::from_values(vec![
                ("name", Value::from(name)),
                ("enabled", Value::from(if enabled { "YES" } else { "NO" })),
            ])
        })
        .collect()
}

pub(crate) fn context_rows() -> Vec<Result<Vec<Rec>>> {
    context_rows_at("2026-11-13T10:00:00.123456Z")
}

pub(crate) fn context_rows_at(at: &str) -> Vec<Result<Vec<Rec>>> {
    let mut rows = instance_rows::rows();
    rows[0] = Ok(vec![instance_rows::platform_at("8.4.0", true, at)]);
    rows.push(Ok(consumer_rows(true, true, true)));
    rows
}

pub(crate) fn thread_row(thread: u64, connection: Value, instrumented: bool, current: Value) -> Rec {
    Rec::from_values(vec![
        ("thread_id", Value::UInt(thread)),
        ("connection_id", connection.clone()),
        ("thread_type", Value::from(if connection == Value::NULL { "BACKGROUND" } else { "FOREGROUND" })),
        ("instrumented", Value::from(if instrumented { "YES" } else { "NO" })),
        ("current_bytes", current),
        ("controlled_current_bytes", Value::UInt(0)),
        ("high_water_bytes", Value::UInt(u64::MAX)),
        ("controlled_high_water_bytes", Value::UInt(1024)),
        ("observed_at", Value::from("2026-11-13T10:00:00.123457Z")),
    ])
}

pub(crate) fn event_row(thread: u64, event: u64, end: Value, start: Value, nested: bool) -> Rec {
    Rec::from_values(vec![
        ("thread_id", Value::UInt(thread)),
        ("event_id", Value::UInt(event)),
        ("end_event_id", end),
        ("timer_start", start),
        ("database_name", Value::from("app")),
        ("query_hash", Value::from("0123456789abcdef0123456789abcdef")),
        ("nesting_type", if nested { Value::from("STATEMENT") } else { Value::NULL }),
        ("observed_at", Value::from("2026-11-13T10:00:00.123458Z")),
    ])
}

async fn qualified_context() -> Context {
    context(&mut Read::with(context_rows())).await.unwrap().0
}

#[test]
fn only_the_native_product_and_column_version_are_admitted() {
    assert_eq!(availability(Flavor::Mariadb, "11.8.0-MariaDB", true), Availability::UnsupportedProduct);
    assert_eq!(availability(Flavor::Mysql, "8.0.30", true), Availability::UnsupportedVersion);
    assert_eq!(availability(Flavor::Mysql, "8.0.31", false), Availability::InstrumentationDisabled);
    assert_eq!(availability(Flavor::Mysql, "8.0.31", true), Availability::Available);
    assert_eq!(availability(Flavor::Mysql, "8.4.0", true), Availability::Available);
}

#[tokio::test]
async fn unsupported_products_versions_and_disabled_schema_stop_before_native_memory_sql() {
    for (version, instrumentation) in [("11.8.0-MariaDB", true), ("8.0.30", true), ("8.4.0", false)] {
        let mut reader = Read::with(vec![Ok(vec![instance_rows::platform(version, instrumentation)])]);
        assert!(context(&mut reader).await.is_err());
        assert_eq!(reader.queries.len(), 1);
        assert!(!reader.queries[0].0.contains("server_uuid"));
    }
}

#[test]
fn data_projections_do_not_multiply_work_or_read_application_sql_and_finalized_statement_peaks() {
    for sql in [thread_query(), event_query().to_string()] {
        let sql = sql.to_ascii_uppercase();
        assert!(sql.ends_with("LIMIT ?"));
        for excluded in [
            "WHERE",
            "JOIN",
            "GROUP BY",
            "ORDER BY",
            "SORT",
            "SELECT *",
            "SQL_TEXT",
            "DIGEST_TEXT",
            "MEMORY_SUMMARY",
            "SETUP_INSTRUMENTS",
        ] {
            assert!(!sql.contains(excluded), "{excluded}: {sql}");
        }
    }
    assert!(!event_query().contains("MAX_TOTAL_MEMORY"), "native statement peaks are finalized at statement end");
}

#[tokio::test]
async fn session_gauges_remain_exact_and_current_event_is_only_context_with_its_own_clock() {
    let context = qualified_context().await;
    let mut reader = Read::with(vec![
        Ok(vec![thread_row(120, Value::UInt(75), true, Value::UInt(u64::MAX))]),
        Ok(vec![event_row(120, 5, Value::NULL, Value::UInt(999), false)]),
    ]);
    let observed = read(&mut reader, &context, 2).await.unwrap();
    assert_eq!(observed.sessions[0].session, NativeActivityIdentity { id: "75".into(), generation: "120".into() });
    assert_eq!(observed.sessions[0].current_bytes.as_deref(), Some("18446744073709551615"));
    assert_eq!(observed.sessions[0].controlled_current_bytes.as_deref(), Some("0"));
    assert_eq!(observed.sessions[0].high_water_bytes.as_deref(), Some("18446744073709551615"));
    assert_eq!(observed.current["120"].request.id, "5");
    assert_eq!(observed.current["120"].request.generation, "999");
    assert_ne!(observed.sessions[0].observed_at, observed.current["120"].observed_at);
    assert_eq!(reader.queries.len(), 2);
    assert_eq!(reader.closed, 0);
}

#[tokio::test]
async fn native_null_is_unknown_while_disabled_tracking_never_becomes_a_measured_zero() {
    let mut context = qualified_context().await;
    for (global, thread, instrumented, expected_queries) in
        [(true, true, true, 2), (false, true, true, 1), (true, false, true, 1), (true, true, false, 1)]
    {
        context.consumers.global = global;
        context.consumers.thread = thread;
        let mut reader =
            Read::with(vec![Ok(vec![thread_row(120, Value::UInt(75), instrumented, Value::NULL)]), Ok(vec![])]);
        let observed = read(&mut reader, &context, 2).await.unwrap();
        assert!(observed.sessions[0].current_bytes.is_none());
        assert_eq!(
            observed.sessions[0].controlled_current_bytes.as_deref(),
            if global && thread && instrumented { Some("0") } else { None }
        );
        assert_eq!(reader.queries.len(), expected_queries);
    }
}

#[tokio::test]
async fn own_native_connections_background_threads_and_no_sessions_need_no_statement_scan() {
    let context = qualified_context().await;
    let mut reader = Read::with(vec![Ok(vec![
        thread_row(100, Value::UInt(context.instance.connection_id), true, Value::UInt(10)),
        thread_row(102, Value::NULL, true, Value::UInt(10)),
    ])]);
    let observed = read(&mut reader, &context, 3).await.unwrap();
    assert!(observed.sessions.is_empty());
    assert!(observed.current.is_empty());
    assert_eq!(reader.queries.len(), 1);
}

#[tokio::test]
async fn recycled_connection_ids_do_not_join_event_context_across_native_thread_generations() {
    let context = qualified_context().await;
    let mut reader = Read::with(vec![
        Ok(vec![thread_row(120, Value::UInt(75), true, Value::UInt(10))]),
        Ok(vec![event_row(121, 5, Value::NULL, Value::UInt(999), false)]),
    ]);
    let observed = read(&mut reader, &context, 2).await.unwrap();
    assert_eq!(observed.sessions.len(), 1);
    assert!(observed.current.is_empty());
}

#[tokio::test]
async fn ended_untimed_and_nested_events_do_not_acquire_an_arbitrary_current_statement_link() {
    let context = qualified_context().await;
    let mut reader = Read::with(vec![
        Ok(vec![thread_row(120, Value::UInt(75), true, Value::UInt(10))]),
        Ok(vec![
            event_row(120, 5, Value::UInt(6), Value::UInt(999), false),
            event_row(120, 7, Value::NULL, Value::NULL, false),
            event_row(120, 8, Value::NULL, Value::UInt(1001), true),
        ]),
    ]);
    assert!(read(&mut reader, &context, 3).await.unwrap().current.is_empty());
}

#[tokio::test]
async fn a_disabled_statement_consumer_preserves_session_counters_without_issuing_that_source_query() {
    let mut context = qualified_context().await;
    context.consumers.statements = false;
    let mut reader = Read::with(vec![Ok(vec![thread_row(120, Value::UInt(75), true, Value::UInt(10))])]);
    let observed = read(&mut reader, &context, 2).await.unwrap();
    assert_eq!(observed.sessions[0].current_bytes.as_deref(), Some("10"));
    assert!(!observed.contexts_available);
    assert_eq!(reader.queries.len(), 1);
}

#[tokio::test]
async fn native_limit_witnesses_are_partial_and_never_followed_by_a_paging_or_lookup_query() {
    let context = qualified_context().await;
    let mut reader = Read::with(vec![
        Ok(vec![
            thread_row(120, Value::UInt(75), true, Value::UInt(10)),
            thread_row(121, Value::UInt(76), true, Value::UInt(20)),
        ]),
        Ok(vec![
            event_row(120, 5, Value::NULL, Value::UInt(999), false),
            event_row(121, 6, Value::NULL, Value::UInt(1000), false),
        ]),
    ]);
    let observed = read(&mut reader, &context, 1).await.unwrap();
    assert_eq!(observed.sessions.len(), 1);
    assert!(observed.truncated);
    assert_eq!(reader.queries.len(), 2);
    assert!(reader.queries.iter().all(|(_, parameters, rows)| parameters == &[Value::UInt(2)] && *rows == 2));
}

#[tokio::test]
async fn malformed_unsigned_native_counters_and_duplicate_thread_rows_are_rejected() {
    let context = qualified_context().await;
    for rows in [
        vec![thread_row(120, Value::UInt(75), true, Value::Int(-1))],
        vec![thread_row(120, Value::UInt(75), true, Value::from("18446744073709551616"))],
        vec![
            thread_row(120, Value::UInt(75), true, Value::UInt(10)),
            thread_row(120, Value::UInt(75), true, Value::UInt(10)),
        ],
        vec![Rec::from_values(vec![("thread_id", Value::UInt(120))])],
    ] {
        let mut reader = Read::with(vec![Ok(rows)]);
        assert!(read(&mut reader, &context, 3).await.is_err());
        assert_eq!(reader.queries.len(), 1);
    }
}

#[tokio::test]
async fn every_failed_or_uncertain_read_stops_before_any_followup_or_fallback_source() {
    let context = qualified_context().await;
    for boundary in 0..2 {
        for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::ByteLimit] {
            let mut replies = vec![Ok(vec![thread_row(120, Value::UInt(75), true, Value::UInt(10))]), Ok(vec![])];
            replies[boundary] = Err(stop.error());
            let mut reader = Read::with(replies);
            assert_eq!(read(&mut reader, &context, 2).await.err().unwrap().detail, stop.error().detail);
            assert_eq!(reader.queries.len(), boundary + 1);
        }
    }
}
