//! Genuine normal reads use the existing nonce admission and fixture lifecycle.
use super::{case, owned_fixture};
use crate::{
    connect::{COLLECTOR, Connector},
    read::{ApplicationReadScope, NativeRead, QueryRead, ReadSession},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Provider, SnapshotOptions, telemetry::*};
use std::time::Duration;

async fn specification(cancel: &CollectionCancellation) -> culprit_core::ConnectionSpec {
    case::require_active(cancel).unwrap();
    owned_fixture::specification(&super::spec().database.unwrap(), CollectionBudget::default()).await
}
#[tokio::test]
#[ignore = "requires the existing nonce-owned PostgreSQL supervisor fixture"]
async fn owned_normal_factory_probe_and_snapshot_settle_their_original_native_readers() {
    let cancel = CollectionCancellation::default();
    let specification = specification(&cancel).await;
    let provider = crate::PostgresProvider::new(&specification).unwrap();
    let completed = case::finish(Duration::from_secs(90), &cancel, async {
        let server = case::admitted(&cancel, provider.probe()).await?;
        let options = SnapshotOptions { plans: 1, top_queries: 32, ..Default::default() };
        let snapshot = case::admitted(&cancel, provider.snapshot("owned-normal-readers", &options)).await?;
        Ok::<_, culprit_core::Error>((server, snapshot))
    })
    .await;
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    assert!(!completed.deadline_reached);
    let (server, snapshot) = completed.value.unwrap();
    assert_eq!(server.database, specification.database.unwrap());
    assert_eq!(snapshot.server.database, server.database);
    assert!(snapshot.identity.is_some());
    assert!(!snapshot.tables.is_empty(), "the real catalog must retain the owned fixture tables");
    assert!(
        snapshot.workload.as_ref().is_some_and(|workload| workload.queries.iter().any(|query| query.plan.is_some())),
        "the seeded native workload must traverse the original planning and parameterized table-metadata route"
    );
}
#[tokio::test]
#[ignore = "requires the existing nonce-owned PostgreSQL supervisor fixture"]
async fn owned_normal_native_parameters_sqlstate_and_trust_cleanup_keep_one_original_session() {
    let cancel = CollectionCancellation::default();
    let specification = specification(&cancel).await;
    let connection = Connector::new(&specification).unwrap();
    let scope = ApplicationReadScope::until(tokio::time::Instant::now() + Duration::from_secs(15));
    let mut reader = ReadSession::application(connection.clone(), COLLECTOR, scope);
    let names = vec!["ordinary_name".to_owned(), "x'; SELECT pg_catalog.pg_sleep(30); --".to_owned()];
    let completed = case::finish(Duration::from_secs(20),&cancel,async {
        let result = async {
            let settings = case::admitted(&cancel,reader.query("SELECT current_setting('statement_timeout') AS statement_limit,current_setting('lock_timeout') AS lock_limit,current_setting('default_transaction_read_only') AS read_only,current_setting('search_path') AS path,current_setting('work_mem') AS work_mem,pg_backend_pid()::text AS pid",1)).await?;
            let parameters = case::admitted(&cancel,reader.query_text_array("SELECT p.name::text AS name,pg_backend_pid() AS pid FROM unnest($1::text[]) WITH ORDINALITY AS p(name,ordinal) ORDER BY p.ordinal",names.clone(),2)).await?;
            let prerequisite = case::admitted(&cancel,async { Ok(reader.query_with_state("DO $culprit$ BEGIN RAISE EXCEPTION USING ERRCODE='55000',MESSAGE='owned prerequisite state'; END $culprit$",1).await) }).await?;
            let synchronized = case::admitted(&cancel,reader.query("SELECT pg_backend_pid()::text AS pid",1)).await?;
            case::admitted(&cancel,crate::trust::begin(&mut reader,false)).await?;
            let local_memory = if settings.first().and_then(|row|row.text("work_mem")).as_deref()==Some("1MB") { "2MB" } else { "1MB" };
            let local_sql = if local_memory=="1MB" { "SET LOCAL work_mem = '1MB'" } else { "SET LOCAL work_mem = '2MB'" };
            case::admitted(&cancel,reader.batch_execute(local_sql)).await?;
            let transaction = case::admitted(&cancel,reader.query("SELECT current_setting('transaction_read_only') AS read_only,current_setting('work_mem') AS work_mem,pg_backend_pid()::text AS pid",1)).await?;
            case::require_active(&cancel)?;
            crate::trust::end(&mut reader).await;
            let released = case::admitted(&cancel,reader.query("SELECT pg_backend_pid()::text AS pid,current_setting('work_mem') AS work_mem",1)).await?;
            Ok::<_,culprit_core::Error>((settings,parameters,prerequisite,synchronized,transaction,released,local_memory))
        }.await;
        let shutdown = reader.close().await;
        (result,shutdown,reader.queries(),reader.metrics().unwrap())
    }).await;
    let (result, shutdown, queries, metrics) = completed.value;
    assert!(!completed.deadline_reached);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(connection.source_shutdown(), SourceShutdown::Quiescent);
    owned_fixture::assert_settled(&metrics, queries);
    assert_eq!(queries, 12, "one setup plus eleven explicit native query/control batches");
    let (settings, parameters, prerequisite, synchronized, transaction, released, local_memory) = result.unwrap();
    let pid = settings[0].text("pid").unwrap();
    assert_eq!(settings[0].text("statement_limit").as_deref(), Some("20s"));
    assert_eq!(settings[0].text("lock_limit").as_deref(), Some("2s"));
    assert_eq!(settings[0].text("read_only").as_deref(), Some("on"));
    assert_eq!(settings[0].text("path").as_deref(), Some("pg_catalog, pg_temp"));
    assert_eq!(parameters.len(), names.len());
    for (row, name) in parameters.iter().zip(names) {
        assert_eq!(row.try_get::<_, String>("name").unwrap(), name);
        assert_eq!(row.try_get::<_, i32>("pid").unwrap().to_string(), pid);
    }
    let Err(failure) = prerequisite else { panic!("a genuine native prerequisite error is required") };
    assert_eq!(failure.sql_state.as_ref().map(|state| state.code()), Some("55000"));
    for row in [&synchronized[0], &transaction[0], &released[0]] {
        assert_eq!(row.text("pid").as_deref(), Some(pid.as_str()));
    }
    assert_eq!(transaction[0].text("read_only").as_deref(), Some("on"));
    assert_eq!(transaction[0].text("work_mem").as_deref(), Some(local_memory));
    assert_eq!(
        released[0].text("work_mem"),
        settings[0].text("work_mem"),
        "the original transaction rollback restores the native session value"
    );
    let id = digest(crate::connect::session_setup(COLLECTOR).as_bytes());
    assert_eq!(metrics.templates.iter().find(|template| template.template_id == id).unwrap().completed_tasks, 1);
    assert_eq!(metrics.templates.iter().map(|template| template.failures).sum::<u32>(), 1);
}
#[tokio::test]
#[ignore = "requires the existing nonce-owned PostgreSQL supervisor fixture"]
async fn owned_normal_dropped_query_retains_its_owner_without_a_replacement_login() {
    let cancel = CollectionCancellation::default();
    let specification = specification(&cancel).await;
    let connection = Connector::new(&specification).unwrap();
    let scope = ApplicationReadScope::until(tokio::time::Instant::now() + Duration::from_secs(10));
    let mut reader = ReadSession::application(connection.clone(), COLLECTOR, scope);
    let completed = case::finish(Duration::from_secs(15), &cancel, async {
        let result = async {
            case::admitted(&cancel, reader.query("SELECT 1 AS warm", 1)).await?;
            case::require_active(&cancel)?;
            let dropped =
                tokio::time::timeout(Duration::from_millis(100), reader.query("SELECT pg_catalog.pg_sleep(30)", 1))
                    .await
                    .is_err();
            let before = reader.queries();
            let replacement = case::admitted(&cancel, reader.query("SELECT 2 AS replacement", 1)).await;
            Ok::<_, culprit_core::Error>((dropped, before, replacement, reader.queries()))
        }
        .await;
        let shutdown = reader.close().await;
        (result, shutdown, reader.queries(), reader.metrics().unwrap())
    })
    .await;
    let (result, shutdown, queries, metrics) = completed.value;
    assert!(!completed.deadline_reached);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(connection.source_shutdown(), SourceShutdown::Quiescent);
    owned_fixture::assert_settled(&metrics, queries);
    let (dropped, before, replacement, after) = result.unwrap();
    assert!(dropped);
    let Err(error) = replacement else { panic!("an original active response cannot be replaced") };
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!((queries, before, after), (3, 3, 3));
    assert_eq!(metrics.templates.iter().map(|template| template.cancellations).sum::<u32>(), 1);
}

#[tokio::test]
#[ignore = "requires the existing nonce-owned PostgreSQL supervisor fixture"]
async fn owned_parameterized_native_rows_obey_each_original_row_and_value_byte_bound() {
    let cancel = CollectionCancellation::default();
    let specification = specification(&cancel).await;
    for (values, limit, expected) in [
        (vec!["first".to_owned(), "second".to_owned()], 1, CollectionStop::RowLimit),
        (vec!["x".repeat(1025)], 1, CollectionStop::ByteLimit),
    ] {
        case::require_active(&cancel).unwrap();
        let scope = ApplicationReadScope::until(tokio::time::Instant::now() + Duration::from_secs(10));
        let mut reader = ReadSession::new(
            Connector::new(&specification).unwrap(),
            CollectionBudget { page_bytes: 1024, ..Default::default() },
        )
        .under_scope(scope);
        let result = case::admitted(
            &cancel,
            reader.query_text_array("SELECT v.name::text AS name FROM unnest($1::text[]) AS v(name)", values, limit),
        )
        .await;
        let shutdown = reader.close().await;
        let metrics = reader.metrics().unwrap();
        assert_eq!(shutdown, SourceShutdown::Quiescent);
        owned_fixture::assert_settled(&metrics, reader.queries());
        assert_eq!(reader.queries(), 2);
        let Err(error) = result else { panic!("bounded native row mode must refuse excess data") };
        assert_eq!(CollectionStop::from_error(&error), expected);
    }
}
