use super::*;
use crate::read::NativeRead;
use culprit_core::ResetMarker;

struct Reader {
    reply: Option<Result<Vec<Row>>>,
    calls: Vec<(String, usize)>,
    paused: bool,
    closed: bool,
}
impl Reader {
    fn new(reply: Result<Vec<Row>>) -> Self {
        Self { reply: Some(reply), calls: vec![], paused: false, closed: false }
    }
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, limit: usize) -> Result<Vec<Row>> {
        self.calls.push((sql.into(), limit));
        if self.paused {
            return std::future::pending().await;
        }
        self.reply.take().expect("one native read, without retry")
    }
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    fn queries(&self) -> u32 {
        self.calls.len() as u32
    }
    fn metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed = true;
        SourceShutdown::Unconfirmed
    }
}

const STARTED_AT: &str = "2020-01-01T05:00:00Z";
fn row(name: Option<&str>, offset: Option<&str>, size: Option<&str>, allocated: Option<&str>) -> Row {
    Row::from_fields([(
        "native_values".into(),
        Some(
            serde_json::json!({
                "name":name,"off":offset,"size":size,"allocated_size":allocated
            })
            .to_string(),
        ),
    )])
}

fn decode(row: &Row, started_at: &str) -> Result<NativeCounterSet> {
    decode_values(&row.text("native_values").unwrap(), started_at)
}

#[tokio::test]
async fn native_allocation_access_uses_effective_role_and_object_privileges_without_changing_them() {
    for (can_read, expected) in [("t", Access::Available), ("f", Access::Restricted)] {
        let mut reader = Reader::new(Ok(vec![Row::from_fields([("can_read".into(), Some(can_read.into()))])]));
        assert!(access(&mut reader, 180_000).await.unwrap() == expected);
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.calls[0].1, 2);
        let query = &reader.calls[0].0;
        assert!(query.contains("'USAGE'") && !query.contains("'MEMBER'"));
        assert!(query.contains("has_table_privilege") && query.contains("has_function_privilege"));
        assert!(!query.contains("GRANT") && !query.contains("SET ROLE"));
    }
    let mut reader = Reader::new(Ok(vec![]));
    assert!(access(&mut reader, 120_000).await.unwrap() == Access::Unsupported);
    assert!(reader.calls.is_empty());
}

#[tokio::test]
async fn shared_segment_allocations_preserve_native_unknowns_without_claiming_backend_or_query_memory() {
    let mut reader = Reader::new(Ok(vec![
        row(Some("Buffer Blocks"), Some("8192"), Some("9007199254740993"), Some("9007199254740994")),
        row(Some("<anonymous>"), None, Some("42"), Some("42")),
        row(None, Some("4096"), Some("0"), Some("0")),
        row(Some("Unknown size"), Some("16384"), None, Some("8192")),
    ]));
    let samples = read(&mut reader, 180_000, STARTED_AT, 20).await.unwrap();
    assert_eq!(samples.len(), 4);
    assert_eq!(samples[0].properties["name"], "Buffer Blocks");
    assert_eq!(samples[0].counters[0].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(samples[1].properties["name"], "<anonymous>");
    assert!(!samples[1].properties.contains_key("off"));
    assert!(!samples[2].properties.contains_key("name"), "free native memory is not named as an allocation owner");
    assert_eq!(samples[2].counters[0].value.integer(), Some(0));
    assert!(!samples[3].counters.iter().any(|counter| counter.key == "size"));
    assert!(samples.iter().all(|sample| sample.is_valid() && sample.scope == MeasurementScope::Server));
    assert!(samples.iter().flat_map(|sample| &sample.counters).all(|counter| counter.unit == CounterUnit::Bytes
        && counter.nature == CounterNature::Gauge
        && counter.scope == MeasurementScope::Server
        && counter.reset == ResetMarker::Unknown));
    let sql = &reader.calls[0].0;
    assert!(sql.contains("pg_catalog.pg_shmem_allocations") && sql.ends_with("LIMIT 21"));
    assert!(!sql.contains("pg_backend_memory_contexts") && !sql.contains("pg_log_backend_memory_contexts"));
    assert!(!samples.iter().any(|sample| sample.identity.qualifiers.contains_key("query_id")));
    for column in FIELDS {
        assert_eq!(sql.matches(&format!("'{column}',m.{column}::text")).count(), 1);
    }
}

#[tokio::test]
async fn allocation_identity_uses_exact_native_name_offset_and_postmaster_epoch_without_metric_values() {
    let at = canonical_time(STARTED_AT).unwrap();
    let first = decode(&row(Some("Buffer Blocks"), Some("8192"), Some("8"), Some("16")), &at).unwrap();
    let changed_values = decode(&row(Some("Buffer Blocks"), Some("8192"), Some("12"), Some("16")), &at).unwrap();
    assert_eq!(first.identity, changed_values.identity);
    for other in [
        row(Some("Buffer Blocks "), Some("8192"), Some("8"), Some("16")),
        row(Some("Buffer Blocks"), Some("16384"), Some("8"), Some("16")),
    ] {
        assert_ne!(first.identity, decode(&other, &at).unwrap().identity);
    }
    assert_ne!(
        first.identity,
        decode(&row(Some("Buffer Blocks"), Some("8192"), Some("8"), Some("16")), "2020-01-01T06:00:00Z")
            .unwrap()
            .identity
    );
    assert_ne!(
        decode(&row(None, None, Some("0"), Some("0")), &at).unwrap().identity,
        decode(&row(Some(""), None, Some("0"), Some("0")), &at).unwrap().identity
    );
}

#[tokio::test]
async fn absent_sources_permissions_invalid_fields_and_overflow_cannot_become_healthy_zero_memory() {
    let mut reader = Reader::new(Ok(vec![]));
    assert_eq!(read(&mut reader, 120_000, STARTED_AT, 20).await.unwrap_err().code, ErrorCode::Unsupported);
    assert!(reader.calls.is_empty());
    assert!(read(&mut reader, 180_000, "not-a-timestamp", 20).await.is_err());
    assert!(reader.calls.is_empty());
    assert!(read(&mut reader, 180_000, STARTED_AT, 0).await.is_err());
    assert!(reader.calls.is_empty());
    let mut reader = Reader::new(Err(Error::new(ErrorCode::PermissionDenied, "fixture denied")));
    assert_eq!(read(&mut reader, 180_000, STARTED_AT, 20).await.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(reader.calls.len(), 1);
    for invalid in [
        row(Some("bad size"), Some("1"), Some("-1"), Some("1")),
        row(Some("bad size"), Some("1"), Some("NaN"), Some("1")),
        row(Some("bad size"), Some("1"), Some("18446744073709551616"), Some("1")),
        row(Some("bad offset"), Some("-1"), Some("1"), Some("1")),
    ] {
        let mut reader = Reader::new(Ok(vec![row(Some("valid"), Some("0"), Some("1"), Some("1")), invalid]));
        assert!(read(&mut reader, 180_000, STARTED_AT, 20).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let duplicate = row(Some("same allocation"), Some("0"), Some("1"), Some("1"));
    let mut reader = Reader::new(Ok(vec![duplicate.clone(), duplicate.clone()]));
    assert!(read(&mut reader, 180_000, STARTED_AT, 20).await.is_err());
    let mut reader = Reader::new(Ok(vec![duplicate.clone(), duplicate]));
    assert_eq!(
        CollectionStop::from_error(&read(&mut reader, 180_000, STARTED_AT, 1).await.unwrap_err()),
        CollectionStop::RowLimit
    );
    assert_eq!(reader.calls[0].1, 2);
}

#[tokio::test]
async fn caller_cancellation_never_turns_unconfirmed_native_work_into_a_successful_sample() {
    let mut reader = Reader::new(Ok(vec![]));
    reader.paused = true;
    {
        let pending = read(&mut reader, 180_000, STARTED_AT, 20);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(1), &mut pending).await.is_err());
    }
    assert_eq!(reader.calls.len(), 1);
    assert!(!reader.closed);
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert!(reader.closed);
}

#[tokio::test]
#[ignore = "requires the owned loopback PostgreSQL provider fixture"]
async fn live_native_shared_allocations_exclude_backend_memory_and_keep_permission_failures_explicit() {
    use crate::read::ReadSession;
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
    let mut reader = ReadSession::new(provider.connector.clone(), CollectionBudget::default());
    let metadata = crate::native::metadata::read(&mut reader, "127.0.0.1", specification.port.unwrap()).await.unwrap();
    let result = read(&mut reader, metadata.version, &metadata.started_at, MAX_ALLOCATIONS).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let samples = result.unwrap();
    assert!(!samples.is_empty());
    assert!(samples.iter().any(|sample| sample.properties.get("name").is_some_and(|name| name == "Buffer Blocks")));
    assert!(samples.iter().all(|sample| sample.scope == MeasurementScope::Server));
    let admin = provider.connector.connect_unconfigured().await.unwrap().client;
    let count_query = "SELECT coalesce(sum(calls),0)::bigint FROM public.pg_stat_statements WHERE query LIKE '%FROM pg_catalog.pg_shmem_allocations m%' AND query NOT LIKE '%pg_stat_statements%'";
    let before = admin.query_one(count_query, &[]).await.unwrap().get::<_, i64>(0);
    let default =
        provider.snapshot("memory-default", &SnapshotOptions { plans: 0, ..Default::default() }).await.unwrap();
    let after = admin.query_one(count_query, &[]).await.unwrap().get::<_, i64>(0);
    assert_eq!(before, after, "default scans must not send the unqualified shared-memory query");
    assert!(default.pressure.as_ref().is_none_or(|pressure| pressure.resource_counters.is_none()));
    let skipped = default.sources.iter().find(|source| source.source == SOURCE_NAME).unwrap();
    assert_eq!(skipped.state, culprit_core::SourceState::NotApplicable);
    let options = SnapshotOptions { plans: 0, collect_shared_resources: true, ..Default::default() };
    let scan = provider.snapshot("memory-opt-in", &options).await.unwrap();
    let projected = scan.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert!(!projected.is_empty());
    assert!(projected.iter().all(|sample| sample.scope == MeasurementScope::Server));
    assert_eq!(
        scan.sources.iter().find(|source| source.source == SOURCE_NAME).unwrap().state,
        culprit_core::SourceState::Collected
    );
    let mut limited = specification;
    limited.user = Some(std::env::var("CULPRIT_SMOKE_LIMITED_USER").unwrap());
    limited.password = Some(std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").unwrap());
    let limited = crate::PostgresProvider::new(&limited).unwrap();
    let mut reader = ReadSession::new(limited.connector.clone(), CollectionBudget::default());
    let result = read(&mut reader, metadata.version, &metadata.started_at, MAX_ALLOCATIONS).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
    let scan = limited.snapshot("memory-restricted", &options).await.unwrap();
    assert!(scan.pressure.as_ref().is_none_or(|pressure| pressure.resource_counters.is_none()));
    assert_eq!(
        scan.sources.iter().find(|source| source.source == SOURCE_NAME).unwrap().state,
        culprit_core::SourceState::Unavailable
    );
    assert_eq!(std::env::var("CULPRIT_SMOKE_LIMITED_USER").unwrap(), "smoke_reader");
    admin.batch_execute("ALTER ROLE smoke_reader NOINHERIT; GRANT pg_read_all_stats TO smoke_reader").await.unwrap();
    let mut reader = ReadSession::new(limited.connector.clone(), CollectionBudget::default());
    let metadata = crate::native::metadata::read(&mut reader, "127.0.0.1", limited.connector.port).await.unwrap();
    let access = access(&mut reader, metadata.version).await.unwrap();
    let shutdown = reader.close().await;
    admin.batch_execute("REVOKE pg_read_all_stats FROM smoke_reader; ALTER ROLE smoke_reader INHERIT").await.unwrap();
    assert!(
        !metadata.full_visibility,
        "membership without effective privileges cannot establish full native visibility"
    );
    assert!(access == Access::Restricted);
    assert_eq!(shutdown, SourceShutdown::Quiescent);
}
