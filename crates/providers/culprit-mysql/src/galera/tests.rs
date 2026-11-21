use super::*;
use crate::{native::galera::tests as rows, read::test_support::Read};
use culprit_core::{ConnectionSpec, Engine, Provider};
use mysql_async::Value;
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: rows::resource() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Galera },
        budget: budget(),
    }
}
fn reader() -> Reader<Read> {
    let mut replies = rows::frame();
    replies.extend(rows::context_rows("2026-11-13T10:00:00.123459Z"));
    Reader { io: Read::with(replies), context: None, until: None }
}
async fn records(source: &mut dyn TelemetrySource) -> Vec<TelemetryRecord> {
    let queries = source.query_count();
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
    assert_eq!(source.query_count(), queries);
    output
}
#[tokio::test]
async fn explicit_real_factory_recipe_never_collects_during_construction_or_approves_cost() {
    let provider = crate::MysqlProvider::new(&ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "127.0.0.1".into(),
        port: Some(1),
        user: Some("unused".into()),
        password: Some("unused".into()),
        ..Default::default()
    })
    .unwrap();
    let mut source = provider.telemetry_source(&request()).unwrap();
    assert_eq!(source.query_count(), 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let recipe = crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mariadb.galera").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.budget.max_queries, 17);
    let mut wrong = request();
    wrong.subject = CollectionSubject::Database;
    assert!(provider.telemetry_source(&wrong).is_err());
}
#[tokio::test]
async fn original_native_point_pages_without_requery_and_final_owner_closes_the_same_reader() {
    let mut source = FrozenSource::new(reader(), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    assert!(galera_context(&description).unwrap().has_qualified_local_owner());
    let rows = records(&mut source).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn setup_and_all_native_contexts_share_one_paid_limit_without_prepared_or_page_work() {
    for max in [17, 16] {
        let mut request = request();
        request.budget.max_queries = max;
        let mut reader = reader();
        reader.io = reader.io.with_query_budget(max, 7).with_native_phases();
        let mut source = FrozenSource::new(reader, &request).unwrap();
        source.describe().await.unwrap();
        records(&mut source).await;
        assert_eq!(source.query_count(), 14);
        if max == 17 {
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), 17);
        } else {
            assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
            assert_eq!(source.query_count(), 16);
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn disabled_unknown_unsupported_and_denied_keep_original_configuration_without_healthy_empty() {
    for case in ["disabled", "unknown", "empty", "denied"] {
        let mut reader = reader();
        if matches!(case, "disabled" | "unknown") {
            for index in [1, 5, 8] {
                reader.io.replies[index] = Ok(vec![crate::rows::Rec::from_values(vec![
                    ("server_uid", Value::from(rows::UID)),
                    ("wsrep_on", if case == "disabled" { Value::from("OFF") } else { Value::NULL }),
                    ("observed_at", Value::from("2026-11-13T10:00:00.123458Z")),
                ])]);
            }
            reader.io.replies.remove(3);
        } else if case == "empty" {
            reader.io.replies[3] = Ok(vec![]);
        } else {
            reader.io.replies[3] = Err(culprit_core::Error::new(
                ErrorCode::PermissionDenied,
                culprit_core::message!("collection.unsupported"),
            ));
        }
        let mut source = FrozenSource::new(reader, &request()).unwrap();
        let description = source.describe().await.unwrap();
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.records, 0, "{case}");
        assert_ne!(snapshot.sources[0].state, SourceState::Empty, "{case}");
        assert!(records(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified, "{case}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_native_socket_member_process_configuration_or_clock_change_withdraws_original() {
    for change in ["member", "socket", "process", "enabled", "clock"] {
        let mut reader = reader();
        let mut final_rows = rows::context_rows("2026-11-13T10:00:00.123459Z");
        let row = |fields: &[(&str, Value)]| crate::rows::Rec::from_values(fields.to_vec());
        match change {
            "member" => {
                final_rows[1] = Ok(vec![row(&[
                    ("server_uid", Value::from("iFL88RG7XPipmQ5HOnA9iLqJw6x=")),
                    ("wsrep_on", Value::from("ON")),
                    ("observed_at", Value::from("2026-11-13T10:00:00.123459Z")),
                ])])
            }
            "socket" => {
                let original = final_rows.remove(0).unwrap();
                let rows =
                    original.into_iter().map(|row| row.with_fields(&[("connection_id", Value::UInt(99))])).collect();
                final_rows.insert(0, Ok(rows));
            }
            "process" => final_rows[2] = Ok(vec![crate::native::error_log::context::tests::process(10001, 100)]),
            "enabled" => {
                final_rows[1] = Ok(vec![row(&[
                    ("server_uid", Value::from(rows::UID)),
                    ("wsrep_on", Value::from("OFF")),
                    ("observed_at", Value::from("2026-11-13T10:00:00.123459Z")),
                ])])
            }
            "clock" => {
                final_rows[1] = Ok(vec![row(&[
                    ("server_uid", Value::from(rows::UID)),
                    ("wsrep_on", Value::from("ON")),
                    ("observed_at", Value::from("2026-11-13T10:00:00.123457Z")),
                ])])
            }
            _ => unreachable!(),
        }
        reader.io.replies.truncate(7);
        reader.io.replies.extend(final_rows);
        let mut source = FrozenSource::new(reader, &request()).unwrap();
        source.describe().await.unwrap();
        records(&mut source).await;
        assert!(!source.finish().await.unwrap().is_verified(), "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn timeout_unconfirmed_and_budget_failures_preserve_first_error_without_more_native_work() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::InvalidPage,
    ] {
        let mut reader = reader();
        reader.io.replies[3] = Err(stop.error());
        let error = reader.observe(&request()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.io.queries.len(), 4);
        assert!(reader.context.is_none());
        if stop == CollectionStop::CancellationUnconfirmed {
            reader.io.shutdown = Some(SourceShutdown::Unconfirmed);
        }
        let shutdown = reader.close().await;
        assert_eq!(reader.io.closed, 1);
        assert_eq!(
            shutdown,
            if stop == CollectionStop::CancellationUnconfirmed {
                SourceShutdown::Unconfirmed
            } else {
                SourceShutdown::Quiescent
            }
        );
    }
}
