use super::*;
use crate::{
    full_text_populations::{StatePermission, tests as fixtures},
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use std::sync::atomic::{AtomicU32, Ordering};
const EXPECTED_SESSION_SETUP_QUERIES: u32 = 1;
struct Reader {
    io: FakeRead,
    closed: Arc<AtomicU32>,
    generation: Option<u64>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.io.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.fetch_add(1, Ordering::SeqCst);
        self.io.close().await
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FullTextPopulations { target: Box::new(fixtures::selection()) },
        budget: CollectionBudget::default(),
    }
}
fn authority(at: &str) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(3),
        &[("selected_database_id", Val::Int(7)), ("observed_at", Val::Text(at.into()))],
    )
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![crate::resources::test_support::instance_pressure_row()]),
        Ok(vec![fixtures::row()]),
        Ok(vec![Rec::of(&[
            ("observed_at", Val::Text("2020-01-01T06:00:00.6Z".into())),
            ("metadata_visibility", Val::Int(1)),
        ])]),
        Ok(vec![replace(&fixtures::row(), &[("observed_at", Val::Text("2020-01-01T06:00:00.8Z".into()))])]),
        Ok(vec![crate::instance::test_support::metadata()]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:01Z")]),
    ]
}
fn source(
    data: Vec<ReadResult<Vec<Rec>>>,
    request: &CollectionRequest,
    generation: Option<u64>,
) -> (FullTextPopulationSource<Reader>, Arc<AtomicU32>) {
    let closed = Arc::new(AtomicU32::new(0));
    let reader = Reader {
        io: FakeRead::new(data).with_query_limit(reader_budget(&request.budget).unwrap().max_queries),
        closed: closed.clone(),
        generation,
    };
    (FullTextPopulationSource::from_reader(reader, request, "original.invalid".into(), 1433).unwrap(), closed)
}
async fn rows(source: &mut FullTextPopulationSource<Reader>) -> Vec<TelemetryRecord> {
    let mut output = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        output.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    output
}
#[tokio::test]
async fn selected_original_rows_finish_on_the_same_native_owner_and_close_once() {
    for writable in [true, false] {
        let mut data = replies();
        if !writable {
            for index in [2, 9] {
                let at = if index == 2 { "2020-01-01T06:00:00Z" } else { "2020-01-01T06:00:01Z" };
                data[index] = Ok(vec![replace(&authority(at), &[("updateability", Val::Text("READ_ONLY".into()))])]);
            }
        }
        let (mut source, closed) = source(data, &request(), Some(1));
        let description = source.describe().await.unwrap();
        let output = rows(&mut source).await;
        assert_eq!(output.len(), 1);
        let TelemetryRecord::Operational(row) = &output[0] else { panic!() };
        let OperationalFact::MssqlFullTextPopulation(point) = &row.fact else { panic!() };
        assert!(point.fits_source(&description));
        assert_eq!(point.status, Some(5));
        assert_eq!(point.context.database_writable, Some(writable));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 10);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn native_empty_denied_unsupported_and_capped_rosters_keep_their_own_coverage_and_finish() {
    for case in ["empty", "denied", "unsupported", "capped"] {
        let mut data = replies();
        let mut request = request();
        request.budget.max_snapshot_rows = 1;
        let mut first = fixtures::row();
        data[4] = Ok(match case {
            "capped" => vec![first.clone(), first],
            _ => {
                first = fixtures::empty_row();
                if case == "denied" {
                    first = replace(&first, &[("can_read_state", Val::Int(0))]);
                }
                if case == "unsupported" {
                    first = replace(&first, &[("layout_available", Val::Int(0))]);
                }
                vec![first]
            }
        });
        let (mut source, closed) = source(data, &request, Some(1));
        let description = source.describe().await.unwrap();
        let captured = rows(&mut source).await;
        let coverage = &description.observation.snapshot().unwrap().sources[0];
        assert_eq!(captured.len(), usize::from(case == "capped"));
        if case == "capped" {
            assert_eq!(coverage.state, culprit_core::SourceState::Partial);
            assert_eq!(coverage.stopped, Some(CollectionStop::RowLimit));
        }
        if case == "denied" {
            assert_eq!(coverage.error_code, Some(ErrorCode::PermissionDenied));
        }
        if case == "unsupported" {
            assert_eq!(coverage.error_code, Some(ErrorCode::Unsupported));
        }
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified, "{case}");
        assert_eq!(source.query_count(), 10);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn unknown_native_role_or_connection_never_dispatches_a_runtime_population_read() {
    for case in ["unknown-role", "unknown-generation"] {
        let mut data = replies();
        if case == "unknown-role" {
            data[2] = Ok(vec![replace(&authority("2020-01-01T06:00:00Z"), &[("updateability", Val::Null)])]);
        }
        let (mut source, closed) = source(data, &request(), if case == "unknown-generation" { None } else { Some(1) });
        assert!(source.describe().await.is_err(), "{case}");
        assert!(source.query_count() <= 3);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn changed_target_process_role_or_regressing_final_clock_cannot_publish_a_verified_original() {
    for case in ["target", "process", "role", "clock"] {
        let mut data = replies();
        match case {
            "target" => {
                data[6] = Ok(vec![replace(
                    &fixtures::row(),
                    &[("catalog_id", Val::Int(999)), ("observed_at", Val::Text("2020-01-01T06:00:00.8Z".into()))],
                )])
            }
            "process" => data[8] = Ok(vec![crate::instance::test_support::epoch(200)]),
            "role" => {
                data[9] = Ok(vec![replace(
                    &authority("2020-01-01T06:00:01Z"),
                    &[("updateability", Val::Text("READ_ONLY".into()))],
                )])
            }
            "clock" => {
                data[6] =
                    Ok(vec![replace(&fixtures::row(), &[("observed_at", Val::Text("2020-01-01T05:59:59Z".into()))])])
            }
            _ => unreachable!(),
        }
        let (mut source, closed) = source(data, &request(), Some(1));
        source.describe().await.unwrap();
        assert_eq!(rows(&mut source).await.len(), 1);
        assert!(!source.finish().await.is_ok_and(|completion| completion.is_verified()), "{case}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn cancellation_keeps_the_same_reader_for_close_and_never_reopens_or_heals_the_capture() {
    let closed = Arc::new(AtomicU32::new(0));
    let io = FakeRead::new(replies()).pause_on_query(5);
    let reader = Reader { io, closed: closed.clone(), generation: Some(1) };
    let mut source =
        FullTextPopulationSource::from_reader(reader, &request(), "original.invalid".into(), 1433).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(5), source.describe()).await.is_err());
    let count = source.query_count();
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), count);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn native_platform_permission_policy_preserves_boxed_managed_instance_and_azure_authority() {
    for (edition, major, expected) in [
        (3, 15, StatePermission::ServerState),
        (3, 16, StatePermission::ServerPerformanceState),
        (8, 15, StatePermission::ServerState),
        (8, 16, StatePermission::ServerPerformanceState),
        (5, 12, StatePermission::AzureDatabase),
    ] {
        let platform = replace(
            &crate::instance::test_support::metadata(),
            &[("edition", Val::Int(edition)), ("major", Val::Text(major.to_string()))],
        );
        let frame = replace(
            &crate::activity::database_context::tests::row(edition),
            &[("major", Val::Int(i64::from(major))), ("observed_at", Val::Text("2020-01-01T06:00:00Z".into()))],
        );
        let data = if edition == 5 {
            vec![Ok(vec![platform]), Ok(vec![frame])]
        } else {
            vec![Ok(vec![platform]), Ok(vec![crate::instance::test_support::epoch(100)]), Ok(vec![frame])]
        };
        let mut reader = Reader { io: FakeRead::new(data), closed: Arc::new(AtomicU32::new(0)), generation: Some(1) };
        let context = Context::read(&mut reader, "original.invalid", 1433).await.unwrap();
        assert_eq!(require_owner(&context, &fixtures::selection()).unwrap(), expected, "edition{edition}/major{major}");
    }
}

#[test]
fn fresh_session_setup_is_reserved_inside_the_clamped_original_caller_query_cap() {
    let upper = CollectionBudget::upper_limits();
    assert_eq!(bounded_budget(&upper).max_queries, MAX_QUERIES);
    assert_eq!(reader_budget(&upper).unwrap().max_queries + EXPECTED_SESSION_SETUP_QUERIES, MAX_QUERIES);
    for total in [2, 6, 10, 11, 12] {
        let requested = CollectionBudget { max_queries: total, ..CollectionBudget::default() };
        assert_eq!(reader_budget(&requested).unwrap().max_queries + EXPECTED_SESSION_SETUP_QUERIES, total);
    }
    let only_setup = CollectionBudget { max_queries: EXPECTED_SESSION_SETUP_QUERIES, ..CollectionBudget::default() };
    assert_eq!(CollectionStop::from_error(&reader_budget(&only_setup).unwrap_err()), CollectionStop::QueryLimit);
}

#[tokio::test]
async fn a_tighter_caller_cannot_fund_a_native_dispatch_with_the_reserved_setup_allowance() {
    for total in [6, 10, 11] {
        let mut requested = request();
        requested.budget.max_queries = total;
        let (mut source, closed) = source(replies(), &requested, Some(1));
        let described = source.describe().await;
        if total == 6 {
            assert_eq!(CollectionStop::from_error(&described.unwrap_err()), CollectionStop::QueryLimit);
        } else {
            described.unwrap();
            assert_eq!(rows(&mut source).await.len(), 1);
            let completion = source.finish().await;
            if total == 11 {
                assert_eq!(completion.unwrap(), SourceCompletion::Verified);
            } else {
                assert_eq!(CollectionStop::from_error(&completion.unwrap_err()), CollectionStop::QueryLimit);
            }
        }
        let dispatched = source.query_count();
        assert_eq!(dispatched + EXPECTED_SESSION_SETUP_QUERIES, total);
        if total < 11 {
            assert!(!source.finish().await.is_ok_and(|completion| completion.is_verified()));
            assert_eq!(source.query_count(), dispatched, "a refused source cannot restart work or renew setup");
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
