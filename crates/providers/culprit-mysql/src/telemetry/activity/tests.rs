use super::*;
use crate::{
    native::activity::test_support::{consumers, row},
    read::test_support::Read,
    rows::Rec,
};
use culprit_core::{ConnectionSpec, Engine};

fn connector() -> Arc<Connector> {
    Arc::new(
        Connector::new(&ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(1),
            database: Some("shop".into()),
            user: Some("unused".into()),
            password: Some("unused".into()),
            ..Default::default()
        })
        .unwrap(),
    )
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget::default(),
    }
}
fn context(at: &str, server: u64) -> Result<Vec<Rec>> {
    Ok(vec![Rec::from_values(vec![
        ("database_name", "shop".into()),
        ("server_id", server.into()),
        ("server_name", "fixture".into()),
        ("connection_id", 7u64.into()),
        ("version", "8.4.0".into()),
        ("login", "reader@%".into()),
        ("performance_schema", 1u64.into()),
        ("read_only", 0u64.into()),
        ("observed_at", at.into()),
    ])])
}
fn epoch() -> Result<Vec<Rec>> {
    Ok(vec![Rec::from_values(vec![("observed_second", 10_000u64.into()), ("uptime", 100u64.into())])])
}
fn visibility() -> Result<Vec<Rec>> {
    Ok(vec![Rec::from_values(vec![("database_wide", 1u64.into()), ("process_wide", 1u64.into())])])
}
fn reader(after: Result<Vec<Rec>>) -> ActivityReader<Read> {
    ActivityReader {
        io: Read::with(vec![
            context("2020-01-01T06:00:00Z", 1),
            epoch(),
            visibility(),
            Ok(consumers(true)),
            Ok(vec![row(1, "Query", true), row(2, "Sleep", false)]),
            after,
            epoch(),
            visibility(),
        ])
        .with_native_phases()
        .with_query_budget(20, 7),
        connector: connector(),
        verified: None,
    }
}
#[tokio::test]
async fn the_complete_native_phase_ledger_fits_twenty_and_frozen_paging_runs_no_optional_work() {
    let request = bounded_request(&request());
    assert_eq!(request.budget.max_queries, 20);
    let mut source = FrozenSource::new(reader(context("2020-01-01T06:00:00.200000Z", 1)), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 20, "includes PREPARE, EXECUTE and all seven native setup phases");
    assert!(description.observation.activity().unwrap().coverage.complete_scope);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ActivityObservations, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 2);
    assert!(next_cursor.is_none());
    assert!(matches!(source.finish().await.unwrap(), SourceCompletion::Verified));
    assert_eq!(source.query_count(), 20, "verification uses frozen before/after evidence");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn source_or_terminal_identity_recheck_failure_withholds_observation_and_stops() {
    for after in [context("2020-01-01T06:00:00.200000Z", 2), Err(CollectionStop::Cancelled.error())] {
        let mut reader = reader(after);
        let error = reader.observe(&request()).await.err().unwrap();
        assert!(matches!(
            CollectionStop::from_error(&error),
            CollectionStop::SourceChanged | CollectionStop::Cancelled
        ));
        assert!(reader.verified.is_none());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn construction_has_no_native_work_and_close_keeps_it_empty() {
    let mut source = source(connector(), &request()).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.query_count(), 0);
}
#[test]
fn tighter_baseline_budget_remains_tighter_for_every_owned_constraint() {
    let original = CollectionRequest {
        budget: CollectionBudget {
            max_queries: 9,
            max_snapshot_rows: 2,
            page_rows: 1,
            statement_timeout_ms: 100,
            total_timeout_ms: 500,
            ..Default::default()
        },
        ..request()
    };
    assert_eq!(bounded_request(&original), original);
}
