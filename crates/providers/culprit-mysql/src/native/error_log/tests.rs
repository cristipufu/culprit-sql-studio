use super::*;
use culprit_core::{Error, telemetry::CollectionStop};
use std::collections::VecDeque;

pub(crate) const AT: u64 = 1_700_000_000_123_456;

pub(crate) fn row(at: u64, thread: Value, code: Value) -> Rec {
    Rec::from_values(vec![
        ("logged_microseconds", Value::UInt(at)),
        ("thread_id", thread),
        ("priority", Value::from("Error")),
        ("error_code", code),
        ("subsystem", Value::from("Server")),
    ])
}

#[derive(Default)]
pub(crate) struct Read {
    pub replies: VecDeque<Result<Vec<Rec>>>,
    pub queries: Vec<(String, Vec<Value>, usize)>,
    pub closed: usize,
    pub shutdown: Option<culprit_core::telemetry::SourceShutdown>,
}

impl Read {
    pub fn with(replies: Vec<Result<Vec<Rec>>>) -> Self {
        Self { replies: replies.into(), ..Default::default() }
    }
}

#[async_trait::async_trait]
impl QueryRead for Read {
    async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<Rec>> {
        self.queries.push((sql.into(), parameters, row_limit));
        self.replies.pop_front().expect("unexpected source SQL")
    }
    async fn explain(&mut self, _: &str) -> Result<Option<Rec>> {
        panic!("native error-log acquisition must never explain application SQL")
    }
}

#[async_trait::async_trait]
impl crate::read::OwnedRead for Read {
    fn query_count(&self) -> u32 {
        self.queries.len() as u32
    }
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> culprit_core::telemetry::SourceShutdown {
        self.closed += 1;
        self.shutdown.unwrap_or(culprit_core::telemetry::SourceShutdown::Quiescent)
    }
}

#[test]
fn structured_metadata_preserves_exact_thread_identity_unknown_codes_and_adjacent_native_microseconds() {
    let first = decode(&row(AT, Value::UInt(u64::MAX), Value::from("MY-999999")), 0).unwrap();
    assert_eq!(first.thread_id.as_deref(), Some("18446744073709551615"));
    assert_eq!(first.error_code.as_deref(), Some("MY-999999"));
    assert_eq!(first.logged_at, "2023-11-14T22:13:20.123456Z");
    assert!(first.message.is_none(), "native DATA is omitted rather than guessed safe");
    let next = decode(&row(AT + 1, Value::UInt(0), Value::NULL), 1).unwrap();
    assert_eq!(next.logged_at, "2023-11-14T22:13:20.123457Z");
    assert_eq!(next.thread_id.as_deref(), Some("0"), "background thread is a native zero, not a query owner");
    assert!(next.error_code.is_none());
    assert_ne!(first.key().unwrap(), next.key().unwrap());
}

#[test]
fn native_nulls_and_duplicates_are_preserved_without_synthesizing_an_event_or_current_session() {
    let row = row(AT, Value::NULL, Value::NULL);
    let first = decode(&row, 0).unwrap();
    let second = decode(&row, 1).unwrap();
    assert!(first.thread_id.is_none());
    assert!(first.error_code.is_none());
    assert_eq!(first.logged_at, second.logged_at);
    assert_ne!(first.key().unwrap(), second.key().unwrap(), "capture-local positions preserve repeated rows");
}

#[test]
fn missing_columns_malformed_text_and_invalid_native_epochs_never_become_zero_or_empty() {
    for malformed in [
        Rec::from_values(vec![]),
        row(0, Value::NULL, Value::NULL),
        row(u64::MAX, Value::NULL, Value::NULL),
        row(AT, Value::Int(-1), Value::NULL),
        row(AT, Value::from("18446744073709551616"), Value::NULL),
        row(AT, Value::NULL, Value::Bytes(vec![0xff])),
    ] {
        assert!(decode(&malformed, 0).is_err());
    }
    let maximum = i32::MAX as u64 * 1_000_000 + 999_999;
    assert_eq!(decode(&row(maximum, Value::NULL, Value::NULL), 0).unwrap().logged_at, "2038-01-19T03:14:07.999999Z");
    assert!(decode(&row(maximum + 1, Value::NULL, Value::NULL), 0).is_err());
}

#[test]
fn data_query_neither_reads_free_form_messages_nor_scans_a_filtered_or_sorted_population() {
    let sql = query().to_ascii_uppercase();
    assert!(sql.contains("UNIX_TIMESTAMP(LOGGED)"));
    assert!(sql.contains("FROM PERFORMANCE_SCHEMA.ERROR_LOG LIMIT ?"));
    for excluded in [" DATA", "WHERE", "ORDER BY", "GROUP BY", "JOIN", "JSON", "PROCESSLIST", "THREADS", "SELECT *"] {
        assert!(!sql.contains(excluded), "{excluded}: {sql}");
    }
}

#[tokio::test]
async fn bounded_native_acquisition_retains_the_prefix_and_marks_a_limit_witness_without_claiming_history() {
    let mut reader = Read::with(vec![Ok(vec![
        row(AT, Value::NULL, Value::NULL),
        row(AT, Value::NULL, Value::NULL),
        row(AT, Value::NULL, Value::NULL),
    ])]);
    let observed = read(&mut reader, 2).await.unwrap();
    assert_eq!(observed.rows.len(), 2);
    assert!(observed.truncated);
    assert_eq!(observed.rows.iter().map(|row| row.position).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(reader.queries[0].1, [Value::UInt(3)]);
    assert_eq!(reader.queries[0].2, 3);
    assert_eq!(reader.closed, 0, "a borrowed native reader never closes its owner's session");
    assert!(read(&mut reader, 0).await.is_err());
    assert!(read(&mut reader, MAX_ROWS + 1).await.is_err());
    assert_eq!(reader.queries.len(), 1);
}

#[tokio::test]
async fn failed_or_interrupted_native_reads_have_no_alternate_source_query() {
    for error in [
        CollectionStop::TimedOut.error(),
        CollectionStop::CancellationUnconfirmed.error(),
        CollectionStop::ByteLimit.error(),
        Error::new(culprit_core::ErrorCode::PermissionDenied, "native permission"),
    ] {
        let expected = error.code;
        let mut reader = Read::with(vec![Err(error)]);
        assert_eq!(read(&mut reader, 2).await.err().unwrap().code, expected);
        assert_eq!(reader.queries.len(), 1);
    }
}

#[test]
fn maria_and_old_mysql_are_not_guessed_to_have_the_new_native_table() {
    assert_eq!(availability(Flavor::Mariadb, "11.8.0-MariaDB", true), Availability::UnsupportedProduct);
    assert_eq!(availability(Flavor::Mysql, "8.0.21", true), Availability::UnsupportedVersion);
    assert_eq!(availability(Flavor::Mysql, "8.0.22", false), Availability::InstrumentationDisabled);
    assert_eq!(availability(Flavor::Mysql, "8.0.22", true), Availability::Available);
    assert_eq!(availability(Flavor::Mysql, "8.4.0", true), Availability::Available);
}
