//! No native server: prove fixture admission, cancellation/failure fencing and actual future sizes.
use super::*;
use crate::read::test_support::Read;
use crate::rows::Rec;

const NONCE: &str = "0123456789abcdef01234567";
fn guard() -> Rec {
    Rec::from_values(vec![
        ("hostname", Value::from(format!("culprit-smoke-mysql-{NONCE}"))),
        ("database_name", Value::from("culprit_smoke")),
        ("connection_id", Value::UInt(41)),
        ("read_only", Value::UInt(0)),
        ("version", Value::from("8.4.0")),
        ("scheduler_setting", Value::from("OFF")),
    ])
}
fn control(replies: Vec<Result<Vec<Rec>>>) -> Control<Read> {
    Control::new(Read::with(replies), NONCE.into(), CollectionCancellation::default())
}
#[tokio::test]
async fn foreign_hostname_database_unknown_session_or_role_never_admits_a_fixture_write() {
    for (field, value) in [
        ("hostname", Value::from("culprit-smoke-mysql-foreign")),
        ("database_name", Value::from("application")),
        ("connection_id", Value::UInt(0)),
        ("read_only", Value::UInt(1)),
        ("version", Value::from("5.6.51")),
        ("version", Value::from("10.6.18-MariaDB")),
        ("scheduler_setting", Value::from("unknown-future-setting")),
    ] {
        let mut owner = control(vec![Ok(vec![guard().with_fields(&[(field, value)])])]);
        assert!(owner.admit().await.is_err(), "{field}");
        assert_eq!(owner.io.queries.len(), 1, "only the owned identity read is dispatched");
        assert!(owner.requires_supervisor_destruction());
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
        assert_eq!(owner.io.closed, 1);
    }
}
#[tokio::test]
async fn known_mariadb_uid_versions_and_scheduler_settings_use_the_same_fixture_owner() {
    for (version, scheduler) in [("8.4.0", "ON"), ("5.7.44", "OFF"), ("11.8.3-MariaDB", "DISABLED")] {
        let mut owner = control(vec![
            Ok(vec![
                guard()
                    .with_fields(&[("version", Value::from(version)), ("scheduler_setting", Value::from(scheduler))]),
            ]),
            Ok(vec![]),
        ]);
        owner.admit().await.unwrap();
        assert_eq!(owner.io.queries.len(), 2);
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
        assert!(!owner.requires_supervisor_destruction());
    }
}
#[tokio::test]
async fn changed_held_session_or_scheduler_setting_refuses_the_next_fixture_ddl() {
    for (field, value) in [("connection_id", Value::UInt(42)), ("scheduler_setting", Value::from("ON"))] {
        let mut owner = control(vec![Ok(vec![guard()]), Ok(vec![]), Ok(vec![guard().with_fields(&[(field, value)])])]);
        owner.admit().await.unwrap();
        assert_eq!(CollectionStop::from_error(&owner.create().await.unwrap_err()), CollectionStop::SourceChanged);
        assert_eq!(owner.io.queries.len(), 3);
        assert!(owner.io.queries.iter().all(|(sql, _, _)| !sql.starts_with("CREATE")));
        assert!(owner.requires_supervisor_destruction());
        owner.cleanup().await.unwrap();
        assert_eq!(owner.io.queries.len(), 3);
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn cancelled_or_uncertain_fixture_work_cannot_dispatch_cleanup_or_replacement_work() {
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let mut cancelled = Control::new(Read::default(), NONCE.into(), cancellation);
    assert_eq!(CollectionStop::from_error(&cancelled.admit().await.unwrap_err()), CollectionStop::Cancelled);
    assert!(cancelled.io.queries.is_empty());
    assert_eq!(cancelled.close().await, SourceShutdown::Quiescent);
    let mut reader = Read::with(vec![
        Ok(vec![guard()]),
        Ok(vec![]),
        Ok(vec![guard()]),
        Err(CollectionStop::CancellationUnconfirmed.error()),
    ]);
    reader.shutdown = Some(SourceShutdown::Unconfirmed);
    let mut uncertain = Control::new(reader, NONCE.into(), CollectionCancellation::default());
    uncertain.admit().await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&uncertain.create().await.unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
    uncertain.cleanup().await.unwrap();
    assert!(uncertain.requires_supervisor_destruction());
    assert_eq!(uncertain.io.queries.len(), 4);
    assert_eq!(uncertain.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(uncertain.io.closed, 1);
}
#[tokio::test]
async fn successful_create_is_dropped_by_the_same_retained_owner_on_a_later_case_failure() {
    let mut owner =
        control(vec![Ok(vec![guard()]), Ok(vec![]), Ok(vec![guard()]), Ok(vec![]), Ok(vec![guard()]), Ok(vec![])]);
    owner.admit().await.unwrap();
    owner.create().await.unwrap();
    assert!(owner.requires_supervisor_destruction());
    owner.cleanup().await.unwrap();
    assert_eq!(owner.io.queries.len(), 6);
    assert!(owner.io.queries.last().unwrap().0.starts_with("DROP EVENT `culprit_scheduled_"));
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    assert!(!owner.requires_supervisor_destruction());
    assert_eq!(owner.io.closed, 1);
}
#[test]
fn actual_owned_control_and_buffered_source_futures_remain_bounded_without_a_native_server() {
    const MAX_FUTURE_BYTES: usize = 4096;
    let specification = culprit_core::ConnectionSpec {
        engine: Some(culprit_core::Engine::Mysql),
        host: "fixture.invalid".into(),
        port: Some(3306),
        database: Some("culprit_smoke".into()),
        user: Some("root".into()),
        ..Default::default()
    };
    let factory = Factory(crate::MysqlProvider::new(&specification).unwrap());
    let cancellation = CollectionCancellation::default();
    let mut owner = Control::new(
        ReadSession::new(factory.0.connector.clone(), fixture::budget(), Scope::Database),
        NONCE.into(),
        cancellation.clone(),
    );
    for (phase, bytes) in [
        ("admission", future_bytes(owner.admit())),
        ("create", future_bytes(owner.create())),
        ("catalog", future_bytes(owner.native_event())),
        ("enable", future_bytes(owner.enable_future())),
        ("drop", future_bytes(owner.drop_event())),
        ("close", future_bytes(owner.close())),
        ("buffered", future_bytes(buffered(&factory, &cancellation))),
    ] {
        assert!(bytes <= MAX_FUTURE_BYTES, "{phase}: {bytes} > {MAX_FUTURE_BYTES}");
    }
    assert!(std::mem::size_of::<Accepted>() <= MAX_FUTURE_BYTES);
    assert!(std::mem::size_of::<original::Original>() <= MAX_FUTURE_BYTES);
    fixture::budget().validate().unwrap();
    assert_eq!(request().budget.max_queries, SOURCE_QUERIES);
    assert_eq!(request().budget.statement_timeout_ms, 2_000);
    assert_eq!(request().budget.total_timeout_ms, 10_000);
}

fn future_bytes<F: std::future::Future>(_: F) -> usize {
    std::mem::size_of::<F>()
}
