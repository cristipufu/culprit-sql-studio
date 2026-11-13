use super::*;
use crate::read::NativeRead;
use culprit_core::ErrorCode;

fn row(source: &str, values: Value) -> Row {
    Row::from_fields([
        ("native_source".into(), Some(source.into())),
        ("native_values".into(), Some(values.to_string())),
    ])
}
fn slot(name: &str) -> Value {
    serde_json::json!({"native_id":name,"name":name,"observed_at":"2026-11-13T08:00:00.123456Z", "native_scope":"server", "active":false,"data_xmin":"1234","data_xmin_age":"2000000", "catalog_xmin":"1200","catalog_xmin_age":"150000000"})
}
fn backend() -> Value {
    serde_json::json!({"native_id":"42","name":"42","observed_at":"2026-11-13T08:00:00.123456Z", "native_scope":"database", "database_oid":"16384", "database":"application", "backend_start":"2026-11-13T06:00:00.123456Z", "transaction_start":"2026-11-13T07:00:00.000001Z", "query_start":"2026-11-13T07:30:00.000002Z", "query_id":"-9223372036854775808", "user_oid":"10", "state":"idle in transaction", "data_xmin":"1234","data_xmin_age":"2000000", "transaction_id":"1240","transaction_id_age":"1999994", "seconds":"3600.5", "program":"application-worker"})
}
struct Reader {
    result: Option<Result<Vec<Row>>>,
    calls: usize,
    pending: bool,
    closed: bool,
}
impl Reader {
    fn new(result: Result<Vec<Row>>) -> Self {
        Self { result: Some(result), calls: 0, pending: false, closed: false }
    }
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        assert!(sql.ends_with(&format!("LIMIT {limit}")));
        self.calls += 1;
        if self.pending {
            return std::future::pending().await;
        }
        self.result.take().expect("one bounded query, no retry")
    }
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    fn queries(&self) -> u32 {
        self.calls as u32
    }
    fn metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed = true;
        SourceShutdown::Quiescent
    }
}

#[test]
fn mixed_native_slot_horizons_and_absent_data_horizon_are_distinct() {
    let mixed = decode(&row("pg_replication_slots", slot("mixed"))).unwrap();
    assert_eq!(mixed.transactions_behind(CleanupHorizon::Data), Some(2_000_000));
    assert_eq!(mixed.transactions_behind(CleanupHorizon::Catalog), Some(150_000_000));
    assert!(!mixed.display(CleanupHorizon::Data).catalog_only);
    assert_eq!(mixed.scope, MeasurementScope::Server);
    assert!(!mixed.identity.qualifiers.contains_key("database_oid"));
    let mut catalog = slot("catalog");
    catalog["data_xmin"] = Value::Null;
    catalog["data_xmin_age"] = Value::Null;
    let catalog = decode(&row("pg_replication_slots", catalog)).unwrap();
    assert_eq!(catalog.transactions_behind(CleanupHorizon::Data), None);
    assert!(catalog.display(CleanupHorizon::Catalog).catalog_only);
}
#[test]
fn backend_transaction_and_previous_statement_have_independent_exact_lifecycles() {
    let held = decode(&row("pg_stat_activity", backend())).unwrap();
    assert_eq!(held.session.as_ref().unwrap().generation, "2026-11-13T06:00:00.123456Z");
    let transaction = held.transaction.as_ref().unwrap();
    assert_eq!(transaction.native_id.as_deref(), Some("1240"));
    assert_eq!(transaction.started_at.as_deref(), Some("2026-11-13T07:00:00.000001Z"));
    let query = held.query.as_ref().unwrap();
    assert_eq!(query.relationship, RetentionQueryRelationship::PreviousStatement);
    assert_eq!(query.identity.as_ref().unwrap().query_id, "-9223372036854775808");
    assert_eq!(query.identity.as_ref().unwrap().qualifiers["database_oid"], "16384");
    assert_eq!(held.code_ownership, PayloadUnavailable::NotCollected);
    assert!(held.horizons.iter().any(|h| h.native_name == "backend_xmin"));
    assert!(held.horizons.iter().any(|h| h.native_name == "backend_xid"));
    let mut next = backend();
    next["transaction_start"] = serde_json::json!("2026-11-13T07:59:00.000003Z");
    assert_ne!(held.key().unwrap(), decode(&row("pg_stat_activity", next)).unwrap().key().unwrap());
}
#[test]
fn missing_generations_stay_unresolved_instead_of_matching_another_session() {
    let mut slot = slot("consumer");
    slot["active"] = Value::Bool(true);
    slot["active_pid"] = serde_json::json!("42");
    let unresolved = decode(&row("pg_replication_slots", slot.clone())).unwrap();
    assert!(unresolved.session.is_none());
    assert_eq!(unresolved.unresolved_session_id.as_deref(), Some("42"));
    slot["backend_start"] = serde_json::json!("2026-11-13T06:00:00.123456Z");
    let resolved = decode(&row("pg_replication_slots", slot)).unwrap();
    assert!(resolved.session.is_some());
    let mut hidden = backend();
    hidden["backend_start"] = Value::Null;
    assert!(decode(&row("pg_stat_activity", hidden)).is_err());
}
#[tokio::test]
async fn bounded_overflow_preserves_a_partial_prefix_and_duplicate_rows_fail() {
    let mut reader =
        Reader::new(Ok(vec![row("pg_replication_slots", slot("a")), row("pg_replication_slots", slot("b"))]));
    let reading = read(&mut reader, Kind::Slot, 180_000, 1).await.unwrap();
    assert!(reading.truncated);
    assert_eq!(reading.holders.len(), 1);
    assert_eq!(reader.calls, 1);
    let duplicate = row("pg_replication_slots", slot("a"));
    let mut reader = Reader::new(Ok(vec![duplicate.clone(), duplicate]));
    assert!(read(&mut reader, Kind::Slot, 180_000, 10).await.is_err());
}
#[tokio::test]
async fn permissions_errors_and_cancellation_remain_owned_by_the_read_session() {
    let mut reader = Reader::new(Err(culprit_core::Error::new(
        ErrorCode::PermissionDenied,
        culprit_core::t!("collection.invalid_source"),
    )));
    assert_eq!(read(&mut reader, Kind::Slot, 180_000, 10).await.err().unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(reader.calls, 1);
    assert!(!reader.closed);
    let mut reader = Reader::new(Ok(vec![]));
    reader.pending = true;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), read(&mut reader, Kind::Session, 180_000, 10))
            .await
            .is_err()
    );
    assert_eq!(reader.calls, 1);
    assert!(!reader.closed);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[test]
fn native_field_overflows_invalid_times_and_query_version_gates_are_explicit() {
    for (field, value) in [
        ("data_xmin", "4294967296"),
        ("data_xmin_age", "18446744073709551616"),
        ("backend_start", "not-a-time"),
        ("seconds", "NaN"),
    ] {
        let mut values = backend();
        values[field] = serde_json::json!(value);
        assert!(decode(&row("pg_stat_activity", values)).is_err(), "{field}");
    }
    assert!(query(Kind::Session, 130_000, 10).contains("NULL::text AS query_id"));
    assert!(query(Kind::Session, 140_000, 10).contains("query_id::text AS query_id"));
    assert!(!capture_query(180_000, false, 10).unwrap().contains("gid AS"));
    assert!(!capture_query(180_000, false, 10).unwrap().contains("pg_waldump"));
}

#[test]
fn recovery_session_scope_does_not_claim_the_unexposed_known_assigned_xid_boundary() {
    assert!(!scope_complete(Kind::Session.source(), true));
    for kind in Kind::ALL {
        assert!(scope_complete(kind.source(), false));
    }
    assert!(scope_complete(Kind::Slot.source(), true));
    assert!(scope_complete(Kind::Prepared.source(), true));
}

#[test]
fn frozen_qualification_metadata_precedes_even_empty_sources_and_round_trips_without_inventing_shared_ownership() {
    for standby in [false, true] {
        let expected = coverage(standby);
        let metadata = Row::from_fields([
            ("native_source".into(), Some(RetentionCoverage::SOURCE.into())),
            ("native_values".into(), Some(serde_json::to_string(&expected).unwrap())),
        ]);
        assert_eq!(decode_coverage(&metadata, standby).unwrap(), expected);
        assert!(decode_coverage(&metadata, !standby).is_err());
        assert!(!expected.qualified_horizons.contains(&CleanupHorizon::SharedCatalog));
        let sql = capture_query(180_000, standby, 2).unwrap();
        assert!(sql.find(RetentionCoverage::SOURCE).unwrap() < sql.find("FROM pg_catalog.pg_stat_activity").unwrap());
        assert!(sources().contains(&RetentionCoverage::SOURCE.into()));
    }
    let held = decode(&row("pg_replication_slots", slot("consumer"))).unwrap();
    let catalog = held.horizons.iter().find(|horizon| horizon.native_name == "catalog_xmin").unwrap();
    assert_eq!(catalog.protects, vec![CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog]);
    assert!(!catalog.protects.contains(&CleanupHorizon::Data));
}

#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn live_native_cleanup_preserves_lifecycles_and_effective_permissions() {
    use crate::connect::{COLLECTOR, INTERACTIVE};
    use culprit_core::{ConnectionSpec, Engine, Provider, SnapshotOptions};
    let specification = ConnectionSpec {
        engine: Some(Engine::Postgres),
        host: "127.0.0.1".into(),
        port: Some(std::env::var("CULPRIT_PREVIEW_PG_PORT").unwrap().parse().unwrap()),
        database: Some(std::env::var("CULPRIT_PREVIEW_PG_DATABASE").unwrap()),
        user: Some("postgres".into()),
        password: Some(std::env::var("CULPRIT_PREVIEW_PG_PASSWORD").unwrap()),
        options: [("sslmode".into(), "disable".into())].into(),
        ..Default::default()
    };
    assert!(matches!(specification.database.as_deref(), Some("culprit_smoke" | "culprit_timescale")));
    let provider = crate::PostgresProvider::new(&specification).unwrap();
    let holder = provider.connector.connect(INTERACTIVE).await.unwrap();
    holder
        .batch_execute(&crate::rows::tagged(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT oid FROM pg_catalog.pg_class LIMIT 1",
        ))
        .await
        .unwrap();
    let pid =
        crate::rows::query_one(&holder, "SELECT pg_backend_pid()::text AS pid").await.unwrap().unwrap().string("pid");
    let mut reader = crate::read::ReadSession::new(provider.connector.clone(), CollectionBudget::default());
    let result = read(&mut reader, Kind::Session, 180_000, MAX_RETENTION_HOLDERS).await;
    let shutdown = reader.close().await;
    let scan = provider.snapshot("retention-fixture", &SnapshotOptions { plans: 0, ..Default::default() }).await;
    holder.batch_execute(&crate::rows::tagged("ROLLBACK")).await.unwrap();
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    let reading = result.unwrap();
    assert!(!reading.truncated);
    let held = reading.holders.iter().find(|held| held.name == pid).unwrap();
    assert_eq!(held.activity, TransactionActivity::IdleInTransaction);
    assert!(held.session.is_some());
    assert!(held.transaction.as_ref().unwrap().started_at.is_some());
    assert_eq!(held.query.as_ref().unwrap().relationship, RetentionQueryRelationship::PreviousStatement);
    assert_eq!(held.code_ownership, PayloadUnavailable::NotCollected);
    let scan = scan.unwrap();
    let retention = scan.pressure.as_ref().unwrap().retention.as_ref().unwrap();
    assert!(retention.is_horizon_complete(CleanupHorizon::Data));
    let projected = retention.holders.iter().find(|row| row.name == pid).unwrap();
    assert_eq!(held.session, projected.session);
    assert_eq!(held.transaction, projected.transaction);
    assert!(retention.holders.iter().all(|row| row.is_valid()));

    let admin = provider.connector.connect_unconfigured().await.unwrap().client;
    assert_eq!(std::env::var("CULPRIT_SMOKE_LIMITED_USER").unwrap(), "smoke_reader");
    admin.batch_execute("ALTER ROLE smoke_reader NOINHERIT; GRANT pg_read_all_stats TO smoke_reader").await.unwrap();
    let mut limited = specification;
    limited.user = Some("smoke_reader".into());
    limited.password = Some(std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").unwrap());
    let limited = crate::PostgresProvider::new(&limited).unwrap();
    let client = limited.connector.connect(COLLECTOR).await.unwrap();
    let probe = crate::probe::probe(&client, "127.0.0.1").await;
    let pressure = match &probe {
        Ok(probe) => Some(crate::pressure::collect(&client, probe).await),
        Err(_) => None,
    };
    admin.batch_execute("REVOKE pg_read_all_stats FROM smoke_reader; ALTER ROLE smoke_reader INHERIT").await.unwrap();
    assert!(!probe.unwrap().privileged, "NOINHERIT membership cannot establish effective source visibility");
    let (pressure, _) = pressure.unwrap();
    let retention = pressure.retention.unwrap();
    assert!(retention.is_valid());
    assert!(!retention.is_horizon_complete(CleanupHorizon::Data));
    assert!(retention.holders.is_empty());
    assert!(retention.sources.iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
}
