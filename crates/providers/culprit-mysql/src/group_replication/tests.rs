use super::*;
use crate::{
    native::{error_log::context::tests as instances, group_replication::tests as rows},
    read::test_support::Read,
};
use culprit_core::{ConnectionSpec, Engine, Provider, SourceState};
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: instances::resource() },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::GroupReplication },
        budget: CollectionBudget { page_rows: 1, ..budget() },
    }
}
fn source_reader() -> Reader<Read> {
    let mut replies = rows::frame();
    replies.extend(rows::context_rows("2026-11-13T10:00:00.123459Z"));
    Reader { io: Read::with(replies), context: None }
}
async fn pages(source: &mut dyn TelemetrySource) -> Vec<TelemetryRecord> {
    let count = source.query_count();
    let mut result = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        result.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(source.query_count(), count);
    result
}
#[tokio::test]
async fn real_factory_construction_discovery_and_recipe_are_explicit_and_cost_unqualified() {
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
    let recipe = crate::telemetry::recipes().into_iter().find(|recipe| recipe.id == "mysql.group-replication").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.budget.max_queries, 23);
    let mut wrong = request();
    wrong.subject = CollectionSubject::Database;
    assert!(provider.telemetry_source(&wrong).is_err());
}
#[tokio::test]
async fn original_frozen_group_read_uses_one_native_decoder_and_no_page_requery() {
    let mut source = FrozenSource::new(source_reader(), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request()));
    assert!(group_replication_context(&description).unwrap().has_qualified_local_owner());
    let rows = pages(&mut source).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(source.query_count(), 10);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 14);
    for record in rows {
        assert!(
            ExportedRecord::new(
                &description,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                record
            )
            .is_ok()
        );
    }
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn prepared_data_reads_and_existing_native_setup_consume_the_same_aggregate_query_allowance() {
    for max in [23, 22] {
        let mut request = request();
        request.budget.max_queries = max;
        let mut reader = source_reader();
        reader.io = reader.io.with_query_budget(max, 7).with_native_phases();
        let mut source = FrozenSource::new(reader, &request).unwrap();
        source.describe().await.unwrap();
        assert_eq!(source.query_count(), 19);
        pages(&mut source).await;
        if max == 23 {
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), 23);
        } else {
            assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
            assert_eq!(source.query_count(), 22);
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn changed_member_process_group_or_layout_at_the_original_final_check_refuses_verification() {
    for change in ["process", "member", "group", "layout"] {
        let mut reader = source_reader();
        match change {
            "process" => reader.io.replies[12] = Ok(vec![instances::process(10000, 99)]),
            "layout" => reader.io.replies[13] = Ok(vec![]),
            _ => {
                let mut frame = rows::context_rows("2026-11-13T10:00:00.123459Z");
                let mut row = frame.remove(1).unwrap().remove(0);
                row = row.with_fields(&[(
                    if change == "member" { "server_uuid" } else { "group_id" },
                    mysql_async::Value::from("cccccccc-0000-4000-8000-000000000003"),
                )]);
                reader.io.replies[11] = Ok(vec![row]);
            }
        }
        let mut source = FrozenSource::new(reader, &request()).unwrap();
        source.describe().await.unwrap();
        pages(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed, "{change}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_native_clock_accepts_later_or_equal_points_and_refuses_a_same_process_backward_point() {
    for (at, expected) in [
        ("2026-11-13T10:00:00.123459Z", SourceCompletion::Verified),
        ("2026-11-13T10:00:00.123458Z", SourceCompletion::Verified),
        ("2026-11-13T10:00:00.123457Z", SourceCompletion::Changed),
    ] {
        let mut replies = rows::frame();
        replies.extend(rows::context_rows(at));
        let reader = Reader { io: Read::with(replies), context: None };
        let mut source = FrozenSource::new(reader, &request()).unwrap();
        source.describe().await.unwrap();
        assert_eq!(source.query_count(), 10);
        assert_eq!(pages(&mut source).await.len(), 2);
        assert_eq!(source.finish().await.unwrap(), expected, "{at}");
        assert_eq!(source.query_count(), 14);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn optional_denial_and_original_partial_counts_remain_independent_while_cancelled_reads_never_run_final_queries()
{
    let mut reader = source_reader();
    reader.io.replies[5] = Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "denied"));
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources[0].state, SourceState::Collected);
    assert_eq!(snapshot.sources[1].state, SourceState::Unavailable);
    assert_eq!(pages(&mut source).await.len(), 1);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut reader = source_reader();
    reader.io.replies[5] = Err(CollectionStop::CancellationUnconfirmed.error());
    reader.io.shutdown = Some(SourceShutdown::Unconfirmed);
    let mut source = FrozenSource::new(reader, &request()).unwrap();
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}

#[tokio::test]
async fn an_insufficient_total_row_allowance_refuses_before_either_native_group_table() {
    let mut request = request();
    request.budget.max_snapshot_rows = 1;
    let mut source = FrozenSource::new(source_reader(), &request).unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_route_label_or_foreign_requested_instance_never_admits_native_group_table_work() {
    let mut request = request();
    let CollectionSubject::Shared { resource } = &mut request.subject else { panic!() };
    resource.native.id = "cccccccc-0000-4000-8000-000000000003".into();
    let mut source = FrozenSource::new(source_reader(), &request).unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
