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
    Ok(vec![Rec::from_values(vec![
        ("database_wide", 1u64.into()),
        ("process_wide", 1u64.into()),
        ("native_member", "f54a461c-0001-4000-8000-000000000001".into()),
    ])])
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
    assert_eq!(description.observation.activity().unwrap().database_writable, Some(true));
    assert!(matches!(&description.partition, SourcePartition::InstanceLocal { member:Some(member),epoch:Some(_) }
        if member.native.id == "f54a461c-0001-4000-8000-000000000001"));
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

#[tokio::test]
async fn a_missing_native_member_retains_partial_activity_and_never_supplies_an_action_owner() {
    let mut reader = reader(context("2020-01-01T06:00:00.200000Z", 1));
    for index in [2, 7] {
        reader.io.replies[index] =
            Ok(vec![visibility().unwrap().remove(0).with_fields(&[("native_member", mysql_async::Value::NULL)])]);
    }
    let capture = reader.observe(&request()).await.unwrap();
    assert_eq!(capture.records.len(), 2);
    assert!(matches!(&capture.description.partition, SourcePartition::InstanceLocal { member: None, epoch: Some(_) }));
    let coverage = &capture.description.observation.activity().unwrap().coverage;
    assert_eq!(coverage.state, culprit_core::SourceState::Partial);
    assert!(!coverage.complete_scope);
    assert_eq!(reader.query_count(), 20);
    assert!(capture.description.partition_is_consistent());
}

#[tokio::test]
async fn a_changed_native_member_withholds_the_whole_original_frame() {
    let mut reader = reader(context("2020-01-01T06:00:00.200000Z", 1));
    reader.io.replies[7] = Ok(vec![
        visibility()
            .unwrap()
            .remove(0)
            .with_fields(&[("native_member", "f54a461c-0001-4000-8000-000000000002".into())]),
    ]);
    let error = reader.observe(&request()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.verified.is_none());
    assert_eq!(reader.query_count(), 20);
}

#[tokio::test]
async fn baseline_member_projection_respects_native_mariadb_variable_versions_in_the_same_query() {
    for (version, member) in [("11.4.2-MariaDB", None), ("11.4.3-MariaDB", Some("q83hHHfky00d9QAi5bwmy+JVlfE="))] {
        let mut reader = reader(context("2020-01-01T06:00:00.200000Z", 1));
        for index in [0, 5] {
            let row = reader.io.replies[index].as_mut().unwrap().remove(0);
            reader.io.replies[index] = Ok(vec![row.with_fields(&[("version", version.into())])]);
        }
        for index in [2, 7] {
            reader.io.replies[index] = Ok(vec![visibility().unwrap().remove(0).with_fields(&[(
                "native_member",
                member.map(mysql_async::Value::from).unwrap_or(mysql_async::Value::NULL),
            )])]);
        }
        let capture = reader.observe(&request()).await.unwrap();
        assert_eq!(reader.query_count(), 20);
        assert_eq!(capture.description.observation.activity().unwrap().coverage.complete_scope, member.is_some());
        let projections =
            reader.io.queries.iter().filter(|(sql, _, _)| sql.contains(" AS native_member")).collect::<Vec<_>>();
        assert_eq!(projections.len(), 2);
        for (sql, _, _) in projections {
            assert_eq!(sql.contains("@@GLOBAL.server_uid AS native_member"), member.is_some());
            assert!(!sql.contains("server_uuid"));
        }
    }
}

#[tokio::test]
async fn foreign_default_schema_and_cancelled_frames_close_the_same_native_owner_without_publishing_a_census() {
    for cancelled in [false, true] {
        let request = bounded_request(&request());
        let mut native = reader(context("2020-01-01T06:00:00.200000Z", 1));
        native.io.replies[4] = if cancelled {
            Err(CollectionStop::Cancelled.error())
        } else {
            Ok(vec![row(1, "Query", true).with_fields(&[("db", "foreign".into())])])
        };
        native.io.shutdown = Some(SourceShutdown::Unconfirmed);
        let mut source = FrozenSource::new(native, &request).unwrap();
        let failure = source.describe().await.err().unwrap();
        let queried = source.query_count();
        assert!(matches!(
            failure.detail,
            Some(culprit_core::ErrorDetail::CollectionStopped(
                CollectionStop::Cancelled | CollectionStop::SourceChanged
            ))
        ));
        assert!(queried <= request.budget.max_queries);
        assert_eq!(
            source.close().await,
            SourceShutdown::Unconfirmed,
            "close must retain the provided native owner, not acquire a replacement"
        );
        assert_eq!(source.query_count(), queried);
    }
}
