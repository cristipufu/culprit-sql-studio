use super::*;
use crate::{
    read::{Parameter, ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, SourceState};
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};
#[derive(Default)]
struct Trace {
    closed: AtomicU32,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}
struct Reader {
    native: FakeRead,
    trace: Arc<Trace>,
    generation: Option<u64>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.trace.calls.lock().unwrap().push((
            sql.into(),
            parameters
                .iter()
                .map(|parameter| match parameter {
                    Parameter::Text(value) => value.clone(),
                    Parameter::Integer(value) => value.to_string(),
                })
                .collect(),
        ));
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
    }
    async fn close(&mut self) -> SourceShutdown {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        self.native.close().await
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Cdc },
        budget: CollectionBudget::default(),
    }
}
fn authority(at: &str) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(3),
        &[("selected_database_id", Val::Int(5)), ("observed_at", Val::Text(at.into()))],
    )
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![crate::cdc::tests::status_row()]),
        Ok(vec![crate::resources::test_support::instance_pressure_row()]),
        Ok(vec![crate::cdc::tests::scan_row()]),
        Ok(vec![crate::resources::test_support::instance_pressure_row()]),
        Ok(vec![crate::cdc::tests::error_row()]),
        Ok(vec![Rec::of(&[
            ("observed_at", Val::Text("2020-01-01T06:00:01Z".into())),
            ("metadata_visibility", Val::Int(1)),
        ])]),
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:02Z")]),
    ]
}
fn source_with_replies(
    rows: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
) -> (ChangeCaptureSource<Reader>, Arc<Trace>) {
    let trace = Arc::new(Trace::default());
    let native = FakeRead::new(rows).with_query_limit(request.budget.max_queries);
    (
        ChangeCaptureSource::with_io(
            Reader { native, trace: trace.clone(), generation },
            request,
            "fixture.invalid".into(),
            1433,
        )
        .unwrap(),
        trace,
    )
}
async fn drain(source: &mut ChangeCaptureSource<Reader>) -> Vec<TelemetryRecord> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            return rows;
        }
    }
}
#[tokio::test]
async fn original_owner_keeps_exact_twelve_dispatches_unknown_reset_and_three_independent_sources() {
    let (mut source, trace) = source_with_replies(replies(), &request(), Some(1));
    let description = source.describe().await.unwrap();
    let rows = drain(&mut source).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 12);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    assert_eq!(description.continuity, SourceContinuity::Unverified);
    assert_eq!(description.target.database().unwrap().incarnation, None);
    for row in rows {
        let TelemetryRecord::Operational(row) = row else { unreachable!() };
        let OperationalFact::MssqlCdc(point) = row.fact else { unreachable!() };
        assert!(point.fits_source(&description));
        assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
    }
    let calls = trace.calls.lock().unwrap();
    let status = calls.iter().find(|(sql, _)| sql.contains("AS scan_layout")).unwrap();
    assert_eq!(status.1, ["VIEW DATABASE PERFORMANCE STATE"]);
    assert!(calls.iter().all(|(sql, _)| !sql.contains("error_message")));
}
#[tokio::test]
async fn disabled_null_permission_and_readonly_do_not_query_or_invent_empty_native_populations() {
    for change in ["disabled", "null", "denied", "readonly"] {
        let mut rows = replies();
        let (key, value) = match change {
            "disabled" => ("enabled", Val::Int(0)),
            "null" => ("enabled", Val::Null),
            _ => ("has_permission", Val::Int(0)),
        };
        if change == "readonly" {
            for index in [2, 11] {
                let original = rows[index].as_ref().unwrap()[0].clone();
                rows[index] = Ok(vec![replace(&original, &[("updateability", Val::Text("READ_ONLY".into()))])]);
            }
        } else {
            rows[3] = Ok(vec![replace(&crate::cdc::tests::status_row(), &[(key, value)])]);
        }
        rows.drain(4..8);
        let (mut source, _) = source_with_replies(rows, &request(), Some(1));
        let description = source.describe().await.unwrap();
        assert_eq!(drain(&mut source).await.len(), 1);
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        let expected = if change == "disabled" { SourceState::NotApplicable } else { SourceState::Unavailable };
        assert!(
            description.observation.snapshot().unwrap().sources.iter().skip(1).all(|source| source.state == expected),
            "{change}"
        );
    }
}
#[tokio::test]
async fn one_denied_dmv_preserves_original_other_facts_but_fences_prevent_followup_sql() {
    let mut rows = replies();
    rows[5] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "native denial")));
    let (mut source, _) = source_with_replies(rows, &request(), Some(1));
    let description = source.describe().await.unwrap();
    assert_eq!(drain(&mut source).await.len(), 2);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(description.observation.snapshot().unwrap().sources[1].error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::SourceChanged,
    ] {
        let mut rows = replies();
        rows[5] = Err(ReadError::Stopped(stop));
        let (mut source, trace) = source_with_replies(rows, &request(), Some(1));
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 6);
        source.close().await;
        assert_eq!(trace.calls.lock().unwrap().len(), 6);
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn error_lookahead_keeps_duplicate_original_positions_and_explicit_partial_coverage() {
    let mut rows = replies();
    rows[7] = Ok(vec![crate::cdc::tests::error_row(); MAX_MSSQL_CDC_ERROR_ROWS + 1]);
    let (mut source, _) = source_with_replies(rows, &request(), Some(1));
    let description = source.describe().await.unwrap();
    let rows = drain(&mut source).await;
    let coverage = &description.observation.snapshot().unwrap().sources[2];
    assert_eq!(coverage.records, 64);
    assert_eq!(coverage.state, SourceState::Partial);
    assert!(!coverage.complete_scope);
    assert_eq!(coverage.stopped, Some(CollectionStop::RowLimit));
    let errors = rows
        .iter()
        .filter_map(|row| match row {
            TelemetryRecord::Operational(row) => match &row.fact {
                OperationalFact::MssqlCdc(point) if matches!(point.data, MssqlCdcData::Error(_)) => {
                    Some(point.position)
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(errors, (1..=64).collect::<Vec<_>>());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn missing_layout_keeps_the_other_native_dmv_and_never_executes_a_fallback() {
    let mut rows = replies();
    rows[3] = Ok(vec![replace(&crate::cdc::tests::status_row(), &[("scan_layout", Val::Int(0))])]);
    rows.drain(4..6);
    let (mut source, trace) = source_with_replies(rows, &request(), Some(1));
    let description = source.describe().await.unwrap();
    assert_eq!(drain(&mut source).await.len(), 2);
    assert_eq!(description.observation.snapshot().unwrap().sources[1].state, SourceState::Unavailable);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(!trace.calls.lock().unwrap().iter().any(|(sql, _)| sql.contains("FROM sys.dm_cdc_log_scan_sessions")));
}
#[tokio::test]
async fn final_role_member_database_or_clock_changes_withhold_completion() {
    for change in ["role", "member", "database", "clock"] {
        let mut rows = replies();
        let index = if change == "member" { 9 } else { 11 };
        let original = rows[index].as_ref().unwrap()[0].clone();
        let (key, value) = match change {
            "role" => ("updateability", Val::Text("READ_ONLY".into())),
            "member" => ("process_id", Val::Int(43)),
            "database" => ("database_guid", Val::Text("other".into())),
            _ => ("observed_at", Val::Text("2019-01-01T06:00:00Z".into())),
        };
        rows[index] = Ok(vec![replace(&original, &[(key, value)])]);
        let (mut source, _) = source_with_replies(rows, &request(), Some(1));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn unknown_original_connection_and_foreign_native_dbid_refuse_without_followup() {
    for generation in [None, Some(0)] {
        let (mut source, _) = source_with_replies(replies(), &request(), generation);
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 3);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut rows = replies();
    rows[5] = Ok(vec![replace(&crate::cdc::tests::scan_row(), &[("database_id", Val::Int(1))])]);
    let (mut source, _) = source_with_replies(rows, &request(), Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn stricter_caller_query_limit_does_not_turn_into_complete_empty_results() {
    let mut limit = request();
    limit.budget.max_queries = 5;
    let (mut source, _) = source_with_replies(replies(), &limit, Some(1));
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 5);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn native_2019_status_requires_database_state_without_the_2022_permission_substitution() {
    let mut rows = replies();
    for index in [0, 9] {
        let original = rows[index].as_ref().unwrap()[0].clone();
        rows[index] = Ok(vec![replace(&original, &[("major", Val::Text("15".into()))])]);
    }
    for index in [2, 11] {
        let original = rows[index].as_ref().unwrap()[0].clone();
        rows[index] = Ok(vec![replace(&original, &[("major", Val::Int(15))])]);
    }
    let (mut source, trace) = source_with_replies(rows, &request(), Some(1));
    source.describe().await.unwrap();
    let rows = drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let calls = trace.calls.lock().unwrap();
    let status = calls.iter().find(|(sql, _)| sql.contains("AS scan_layout")).unwrap();
    assert_eq!(status.1, ["VIEW DATABASE STATE"]);
    assert!(rows.iter().any(|row|matches!(row,TelemetryRecord::Operational(row) if matches!(&row.fact,OperationalFact::MssqlCdc(point) if matches!(&point.data,MssqlCdcData::Status(status) if status.native_major==15 && status.permission==MssqlCdcPermission::ViewDatabaseState)))));
}

#[tokio::test]
async fn one_row_budget_preserves_original_status_and_records_each_unread_population_without_dispatch() {
    let mut rows = replies();
    rows.drain(4..8);
    let mut request = request();
    request.budget.max_snapshot_rows = 1;
    let (mut source, trace) = source_with_replies(rows, &request, Some(1));
    let description = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 5);
    let records = drain(&mut source).await;
    assert_eq!(records.len(), 1);
    assert!(
        matches!(&records[0],TelemetryRecord::Operational(row) if matches!(&row.fact,OperationalFact::MssqlCdc(point) if matches!(&point.data,MssqlCdcData::Status(status) if status.enabled==Some(true))))
    );
    let header = description.observation.snapshot().unwrap();
    assert_eq!(header.records, 1);
    assert_eq!(header.sources[0].state, SourceState::Collected);
    for coverage in header.sources.iter().skip(1) {
        assert_eq!(coverage.state, SourceState::Unavailable);
        assert_eq!(coverage.records, 0);
        assert_eq!(coverage.stopped, Some(CollectionStop::RowLimit));
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let calls = trace.calls.lock().unwrap();
    assert!(!calls.iter().any(|(sql,_)|sql.contains("FROM sys.dm_cdc_log_scan_sessions")||sql.contains("FROM sys.dm_cdc_errors")));
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
}

mod change_feed;
