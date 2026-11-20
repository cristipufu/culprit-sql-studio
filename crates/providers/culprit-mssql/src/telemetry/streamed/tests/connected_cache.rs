use super::*;
use crate::telemetry::{
    cached_definitions::CachedDefinitions,
    cached_runtime::CachedRuntime,
    retained_definitions::{RetainedPlans, bounded_budget},
};

#[derive(Clone, Copy)]
enum Owner {
    Stable,
    Restarted,
    Hidden,
}

fn authority(edition: i64, writable: Option<bool>) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(edition),
        &[
            ("selected_database_id", Val::Int(5)),
            (
                "updateability",
                writable
                    .map(|value| Val::Text(if value { "READ_WRITE" } else { "READ_ONLY" }.into()))
                    .unwrap_or(Val::Null),
            ),
        ],
    )
}

fn base_row(dataset: SnapshotDataset) -> Rec {
    if dataset == SnapshotDataset::CachedRuntime {
        crate::cached_runtime::tests::row()
    } else {
        crate::cached_definitions::tests::row()
    }
}

fn frame_row(dataset: SnapshotDataset, writable: bool) -> Rec {
    let row = base_row(dataset);
    let mut columns = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    columns.push(("updateability", Val::Text(if writable { "READ_WRITE" } else { "READ_ONLY" }.into())));
    Rec::of(&columns)
}

fn native_row(dataset: SnapshotDataset) -> Rec {
    frame_row(dataset, false)
}

fn source<D: SnapshotRecipe>(
    recipe: D,
    edition: i64,
    owner: Owner,
    before: Rec,
    after: Rec,
    row: Rec,
) -> StreamedSnapshot<D, StreamingRead> {
    let metadata = replace(
        &crate::instance::test_support::metadata(),
        &[
            ("edition", Val::Int(edition)),
            ("major", Val::Text(crate::read::integer(&before, "major").unwrap().to_string())),
        ],
    );
    let epoch = || match owner {
        Owner::Hidden => Err(crate::read::ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "owned test epoch is hidden",
        ))),
        _ => Ok(vec![crate::instance::test_support::epoch(100)]),
    };
    let mut replies = vec![Ok(vec![metadata.clone()])];
    if edition != 5 {
        replies.push(epoch());
    }
    replies.push(Ok(vec![before]));
    replies.push(Ok(vec![if edition == 5 {
        pressure()
    } else {
        crate::resources::test_support::instance_pressure_row()
    }]));
    match D::DATASET {
        SnapshotDataset::CachedRuntime => replies.push(Ok(crate::cached_runtime::tests::spill_columns())),
        SnapshotDataset::RetainedPlans => {
            replies.push(Ok(vec![crate::retained_definitions::tests::feature(1, 1)]));
            replies.push(Ok(vec![Rec::of(&[("value", Val::Int(1))])]));
        }
        SnapshotDataset::CachedDefinitions => {}
        _ => panic!("only the three local cache recipes"),
    }
    replies.push(Ok(vec![if edition == 5 {
        pressure()
    } else {
        crate::resources::test_support::instance_pressure_row()
    }]));
    if D::DATASET != SnapshotDataset::CachedRuntime {
        let context = crate::cached_statements::context(&native_row(D::DATASET)).unwrap();
        replies.push(Ok(vec![crate::relational_inputs::tests::binding_row(&context)]));
    }
    if edition != 5 {
        let after_metadata = match owner {
            Owner::Restarted => replace(&metadata, &[("process_id", Val::Int(43))]),
            _ => metadata,
        };
        replies.push(Ok(vec![after_metadata]));
        replies.push(epoch());
    }
    replies.push(Ok(vec![after]));
    let io = StreamingRead {
        native: FakeRead::new(replies),
        batches: vec![batch(row, true)].into(),
        starts: 0,
        pause_batches: false,
    };
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: D::DATASET },
        budget: bounded_budget(&CollectionBudget::default()),
    };
    StreamedSnapshot::with_io(io, &request, "fixture.invalid".into(), 1433, recipe).unwrap()
}

async fn completes<D: SnapshotRecipe>(recipe: D, edition: i64, writable: bool) {
    let native = authority(edition, Some(writable));
    let mut source = source(recipe, edition, Owner::Stable, native.clone(), native, frame_row(D::DATASET, writable));
    assert_eq!(source.query_count(), 0);
    let initial = source.describe().await.unwrap();
    assert_eq!(initial.target.database().unwrap().incarnation.is_some(), edition == 5);
    assert!(matches!(initial.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
    assert!(initial.partition.is_qualified());
    let CollectionRead::Page { records, next_cursor } = source.read_page(D::DATASET.stream(), None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert!(completed.observation.snapshot().unwrap().sources[0].complete_scope);
    let expected = match (edition == 5, D::DATASET) {
        (false, SnapshotDataset::RetainedPlans) => 12,
        (false, _) => 10,
        (true, SnapshotDataset::RetainedPlans) => 9,
        (true, _) => 7,
    };
    assert_eq!(source.query_count(), expected, "same native source roster for either observed role");
    assert_eq!(source.inner.reader().io.starts, 1);
    let calls = &source.inner.reader().io.native.calls;
    let stream = calls.iter().find(|call| call.sql.contains("SELECT TOP (@P1) qs.plan_handle")).unwrap();
    assert!(stream.sql.contains(crate::cached_statements::DATABASE_PREDICATE));
    assert!(!stream.sql.contains("COALESCE(st.dbid") && !stream.sql.contains("ISNULL(st.dbid"));
    if edition != 5 {
        assert!(calls.iter().all(|call| !call.sql.contains("sys.dm_user_db_resource_governance")));
    }
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.inner.reader().io.native.closed, 1);
}

#[tokio::test]
async fn all_three_cache_recipes_admit_known_readable_and_writable_original_native_members() {
    for edition in [2, 3, 4, 5, 8] {
        for writable in [true, false] {
            completes(CachedRuntime, edition, writable).await;
            completes(CachedDefinitions, edition, writable).await;
            completes(RetainedPlans, edition, writable).await;
        }
    }
}

async fn refuses_unknown_owner<D: SnapshotRecipe>(recipe: D, edition: i64, owner: Owner, native: Rec) {
    let mut source = source(recipe, edition, owner, native.clone(), native, native_row(D::DATASET));
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), if edition == 5 { 2 } else { 3 });
    assert_eq!(source.inner.reader().io.starts, 0);
    assert!(source.finish().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unknown_updateability_member_or_native_process_cannot_authorize_a_readable_cache_stream() {
    for edition in [3, 5, 8] {
        for field in ["updateability", if edition == 5 { "replica_id" } else { "database_file_guid" }] {
            let native = replace(&authority(edition, Some(false)), &[(field, Val::Null)]);
            refuses_unknown_owner(CachedRuntime, edition, Owner::Stable, native.clone()).await;
            refuses_unknown_owner(CachedDefinitions, edition, Owner::Stable, native.clone()).await;
            refuses_unknown_owner(RetainedPlans, edition, Owner::Stable, native).await;
        }
    }
    for recipe in [SnapshotDataset::CachedRuntime, SnapshotDataset::CachedDefinitions, SnapshotDataset::RetainedPlans] {
        let native = authority(3, Some(false));
        match recipe {
            SnapshotDataset::CachedRuntime => refuses_unknown_owner(CachedRuntime, 3, Owner::Hidden, native).await,
            SnapshotDataset::CachedDefinitions => {
                refuses_unknown_owner(CachedDefinitions, 3, Owner::Hidden, native).await
            }
            _ => refuses_unknown_owner(RetainedPlans, 3, Owner::Hidden, native).await,
        }
    }
}

async fn refuses_changed_owner<D: SnapshotRecipe>(recipe: D, owner: Owner, before: Rec, after: Rec) {
    let mut source = source(recipe, 3, owner, before, after, native_row(D::DATASET));
    source.describe().await.unwrap();
    source.read_page(D::DATASET.stream(), None).await.unwrap();
    assert!(matches!(source.finish().await.unwrap(), SourceCompletion::Changed));
    assert_eq!(source.inner.reader().io.starts, 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_role_database_recovery_or_process_withdraws_drained_cache_completion() {
    let before = authority(3, Some(false));
    for (owner, after) in [
        (Owner::Restarted, before.clone()),
        (Owner::Stable, authority(3, Some(true))),
        (Owner::Stable, replace(&before, &[("recovery_fork_guid", Val::Text("other-fork".into()))])),
        (Owner::Stable, replace(&before, &[("database_file_guid", Val::Null)])),
        (Owner::Stable, replace(&before, &[("updateability", Val::Null)])),
    ] {
        refuses_changed_owner(CachedRuntime, owner, before.clone(), after.clone()).await;
        refuses_changed_owner(CachedDefinitions, owner, before.clone(), after.clone()).await;
        refuses_changed_owner(RetainedPlans, owner, before.clone(), after).await;
    }
}

async fn refuses_row<D: SnapshotRecipe>(recipe: D, row: Rec) {
    let native = authority(3, Some(false));
    let mut source = source(recipe, 3, Owner::Stable, native.clone(), native, row);
    source.describe().await.unwrap();
    assert!(source.read_page(D::DATASET.stream(), None).await.is_err());
    let calls = source.query_count();
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), calls, "unowned native rows cannot trigger a binding or final authority read");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

async fn refuses_foreign_row<D: SnapshotRecipe>(recipe: D, value: Val) {
    refuses_row(recipe, replace(&native_row(D::DATASET), &[("database_id", value)])).await;
}

async fn refuses_frame_role<D: SnapshotRecipe>(recipe: D, value: Option<Val>) {
    let row = match value {
        Some(value) => replace(&native_row(D::DATASET), &[("updateability", value)]),
        None => base_row(D::DATASET),
    };
    // Initial and final authority are READ_ONLY; a different/unknown/missing frame cannot be repaired by A/A.
    refuses_row(recipe, row).await;
}

#[tokio::test]
async fn an_a_b_a_role_change_or_unavailable_native_frame_cannot_publish_any_cache_family() {
    for role in [Some(Val::Text("READ_WRITE".into())), Some(Val::Null), Some(Val::Text("unknown".into())), None] {
        refuses_frame_role(CachedRuntime, role.clone()).await;
        refuses_frame_role(CachedDefinitions, role.clone()).await;
        refuses_frame_role(RetainedPlans, role).await;
    }
}

#[test]
fn native_frame_role_is_observed_once_and_does_not_change_the_existing_scan_sql() {
    let budget = CollectionBudget::default();
    let cached = crate::cached_definitions::Limits::from_budget(&budget).unwrap();
    let retained = crate::retained_definitions::Layout {
        maximum_bytes: 64 * 1024,
        maximum_statement_bytes: crate::actual::retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    };
    let runtime = crate::cached_runtime::Layout::default();
    for (scan, frame) in [
        (crate::cached_runtime::select_sql(&runtime), crate::cached_runtime::acquisition_sql(&runtime)),
        (crate::cached_definitions::select_sql(&cached), crate::cached_definitions::acquisition_sql(&cached)),
        (crate::retained_definitions::select_sql(&retained), crate::retained_definitions::acquisition_sql(&retained)),
    ] {
        assert!(!scan.contains("@culprit_cache_updateability"));
        assert!(!scan.contains("N'Updateability'"));
        assert!(frame.starts_with("DECLARE /* culprit */ @culprit_cache_updateability"));
        assert_eq!(frame.matches("DATABASEPROPERTYEX(DB_NAME(),N'Updateability')").count(), 1);
        assert_eq!(frame.matches("@culprit_cache_updateability AS updateability").count(), 1);
        assert!(frame.contains(crate::cached_statements::DATABASE_PREDICATE));
    }
}

#[tokio::test]
async fn every_cache_row_requires_the_original_native_database_id_before_proof_or_publication() {
    for database in [Val::Int(1), Val::Int(7), Val::Null, Val::Int(-1)] {
        refuses_foreign_row(CachedRuntime, database.clone()).await;
        refuses_foreign_row(CachedDefinitions, database.clone()).await;
        refuses_foreign_row(RetainedPlans, database).await;
    }
}

#[tokio::test]
async fn cached_definition_json_prerequisite_is_independent_of_runtime_and_retained_feature_layout() {
    let native = replace(&authority(3, Some(false)), &[("major", Val::Int(12))]);
    let mut definitions = source(
        CachedDefinitions,
        3,
        Owner::Stable,
        native.clone(),
        native,
        native_row(SnapshotDataset::CachedDefinitions),
    );
    assert_eq!(definitions.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
    assert_eq!(definitions.query_count(), 3);
    assert_eq!(definitions.inner.reader().io.starts, 0);
    assert_eq!(definitions.close().await, SourceShutdown::Quiescent);
    for edition in [5, 8] {
        assert!(crate::probe::supports_current_type_schema(edition, 12));
    }
    assert!(!crate::probe::supports_current_type_schema(3, 12));
    let native = replace(&authority(3, Some(false)), &[("major", Val::Int(12))]);
    let mut runtime =
        source(CachedRuntime, 3, Owner::Stable, native.clone(), native, native_row(SnapshotDataset::CachedRuntime));
    let initial = runtime.describe().await.unwrap();
    runtime.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert!(initial.completed(runtime.finish().await.unwrap()).unwrap().is_some());
    assert!(runtime.inner.reader().io.native.calls.iter().all(|call| !call.sql.contains("FOR JSON")));
    assert_eq!(runtime.close().await, SourceShutdown::Quiescent);
}

async fn refuses_regressed_clock<D: SnapshotRecipe>(recipe: D) {
    let before = authority(3, Some(false));
    let after = replace(&before, &[("observed_at", Val::Text("2019-12-31T23:59:59Z".into()))]);
    let mut source = source(recipe, 3, Owner::Stable, before, after, native_row(D::DATASET));
    source.describe().await.unwrap();
    source.read_page(D::DATASET.stream(), None).await.unwrap();
    assert!(source.finish().await.is_err());
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn native_clock_regression_cannot_certify_any_drained_cache_family() {
    refuses_regressed_clock(CachedRuntime).await;
    refuses_regressed_clock(CachedDefinitions).await;
    refuses_regressed_clock(RetainedPlans).await;
}

#[tokio::test]
async fn unsupported_native_platform_still_refuses_before_cache_work() {
    for edition in [6, 9, 99] {
        let native = authority(edition, Some(false));
        let mut source = source(
            RetainedPlans,
            edition,
            Owner::Stable,
            native.clone(),
            native,
            native_row(SnapshotDataset::RetainedPlans),
        );
        assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(source.query_count(), 1);
        assert_eq!(source.inner.reader().io.starts, 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[test]
fn retained_source_budget_is_bounded_and_preserves_stricter_caller_limits() {
    let ceiling = bounded_budget(&CollectionBudget::upper_limits());
    ceiling.validate().unwrap();
    assert_eq!((ceiling.page_rows, ceiling.max_snapshot_rows, ceiling.max_queries), (64, 64, 12));
    assert_eq!((ceiling.statement_timeout_ms, ceiling.total_timeout_ms), (1_000, 5_000));
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        max_queries: 1,
        statement_timeout_ms: 100,
        total_timeout_ms: 100,
        ..CollectionBudget::default()
    };
    let bounded = bounded_budget(&strict);
    assert_eq!((bounded.page_rows, bounded.max_snapshot_rows, bounded.max_queries), (1, 1, 1));
    assert_eq!((bounded.statement_timeout_ms, bounded.total_timeout_ms), (100, 100));
}
