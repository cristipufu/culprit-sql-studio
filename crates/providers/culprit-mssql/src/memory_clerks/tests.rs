use super::*;
use crate::{
    read::NativeRead,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{Error, ErrorCode, ResetMarker};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("type", Val::Text("CACHESTORE_SQLCP".into())),
        ("name", Val::Text("SQL Plans".into())),
        ("memory_node_id", Val::Int(0)),
        ("clerk_count", Val::Text("2".into())),
        ("pages_kb", Val::Text("9007199254740993".into())),
        ("virtual_memory_reserved_kb", Val::Text("1024".into())),
        ("virtual_memory_committed_kb", Val::Text("512".into())),
        ("awe_allocated_kb", Val::Text("0".into())),
        ("shared_memory_reserved_kb", Val::Null),
        ("shared_memory_committed_kb", Val::Text("0".into())),
        ("minimum_page_size_in_bytes", Val::Text("8192".into())),
        ("maximum_page_size_in_bytes", Val::Text("65536".into())),
    ])
}

#[tokio::test]
async fn component_allocations_are_exact_shared_gauges_without_query_ownership_or_pointer_export() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let samples = read(&mut reader, 20).await.unwrap();
    let sample = &samples[0];
    assert_eq!(sample.identity.id, "CACHESTORE_SQLCP");
    assert_eq!(sample.properties["name"], "SQL Plans");
    assert_eq!(sample.scope, MeasurementScope::Server);
    assert!(sample.is_valid());
    let counter = |name: &str| sample.counters.iter().find(|counter| counter.key == name).unwrap();
    assert_eq!(counter("pages_kb").value, MeasurementValue::Integer("9007199254740993".into()));
    assert_eq!(counter("pages_kb").unit, CounterUnit::Kibibytes);
    assert_eq!(counter("awe_allocated_kb").value.integer(), Some(0));
    assert_eq!(counter("minimum_page_size_in_bytes").unit, CounterUnit::Bytes);
    assert_eq!(counter("minimum_page_size_in_bytes").value.integer(), Some(8192));
    assert_eq!(counter("maximum_page_size_in_bytes").value.integer(), Some(65536));
    assert!(!sample.counters.iter().any(|counter| counter.key == "shared_memory_reserved_kb"));
    assert!(sample.counters.iter().all(|counter| counter.nature == CounterNature::Gauge
        && counter.reset == ResetMarker::Unknown
        && counter.scope == MeasurementScope::Server));
    assert!(!sample.properties.contains_key("query_id") && !sample.identity.qualifiers.contains_key("query_id"));
    let sql = &reader.calls[0].sql;
    assert!(sql.contains("GROUP BY type_key,name_key,memory_node_id") && sql.contains("TOP (@P1)"));
    assert!(sql.contains("CONVERT(varbinary(512),name)"), "member collation cannot merge native names");
    assert!(
        !sql.contains("memory_clerk_address") && !sql.contains("sys.dm_os_memory_objects") && !sql.contains("DBCC")
    );
    assert_eq!(reader.calls[0].limit, 21);
}

#[test]
fn every_declared_dimension_has_one_projection_decode_and_native_provenance() {
    let sql = query();
    let sample = decode(&row()).unwrap();
    assert_eq!(FIELDS.len(), 9);
    for field in FIELDS {
        assert_eq!(sql.matches(&format!(" AS {}", field.key)).count(), 1, "one field alias for {}", field.key);
        if !matches!(field.aggregation, Aggregation::Count) {
            assert!(field.projection().contains(&format!("COUNT_BIG({})=COUNT_BIG(*)", field.column())));
        }
        if let Some(counter) = sample.counters.iter().find(|counter| counter.key == field.key) {
            assert_eq!(counter.native_name, field.column());
            assert_eq!(counter.native_object, NATIVE_OBJECT);
        }
    }
    assert!(!sql.contains("SUM(page_size_in_bytes)"), "allocation granularity is not memory consumption");
}

#[test]
fn native_group_identity_preserves_names_nodes_and_types_without_using_memory_values() {
    let original = decode(&row()).unwrap();
    assert_eq!(original.identity, decode(&replace(&row(), &[("pages_kb", Val::Text("1".into()))])).unwrap().identity);
    for change in [
        ("name", Val::Text("SQL Plans ".into())),
        ("name", Val::Text("sql plans".into())),
        ("memory_node_id", Val::Int(64)),
        ("type", Val::Text("CACHESTORE_OBJCP".into())),
    ] {
        assert_ne!(original.identity, decode(&replace(&row(), &[change])).unwrap().identity);
    }
    let unnamed = decode(&replace(&row(), &[("name", Val::Text("".into()))])).unwrap();
    assert_eq!(unnamed.properties["name"], "");
    assert!(unnamed.is_valid());
}

#[tokio::test]
async fn malformed_overflowed_and_permission_denied_reads_never_publish_partial_memory() {
    for change in [
        ("type", Val::Null),
        ("name", Val::Null),
        ("memory_node_id", Val::Int(32768)),
        ("pages_kb", Val::Text("-1".into())),
        ("pages_kb", Val::Text("18446744073709551616".into())),
        ("pages_kb", Val::Float(42.0)),
        ("clerk_count", Val::Null),
        ("clerk_count", Val::Text("0".into())),
    ] {
        let other = replace(&row(), &[("name", Val::Text("Another component".into()))]);
        let mut reader = FakeRead::new(vec![Ok(vec![row(), replace(&other, &[change])])]);
        assert!(read(&mut reader, 20).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let mut reader = FakeRead::new(vec![Ok(vec![row(), row()])]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 1).await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(reader.calls[0].limit, 2);
    let mut reader = FakeRead::new(vec![Ok(vec![row(), row()])]);
    assert!(read(&mut reader, 20).await.is_err(), "duplicate native groups cannot become separate owners");
    let mut reader = FakeRead::new(vec![Err(Error::new(ErrorCode::PermissionDenied, "fixture denied").into())]);
    assert_eq!(read(&mut reader, 20).await.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(reader.calls.len(), 1);
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert_eq!(CollectionStop::from_error(&read(&mut reader, 20).await.unwrap_err()), CollectionStop::TimedOut);
    let mut reader = FakeRead::new(vec![]);
    assert!(read(&mut reader, 0).await.is_err());
    assert!(reader.calls.is_empty());
    let mut reader = FakeRead::new(vec![Ok(vec![])]);
    assert_eq!(
        CollectionStop::from_error(&read(&mut reader, 20).await.unwrap_err()),
        CollectionStop::SourceUnavailable,
        "a running engine must not publish an absent clerk view as healthy zero memory"
    );
}

#[tokio::test]
async fn retiring_a_read_requires_the_callers_explicit_native_shutdown() {
    let mut reader = FakeRead::new(vec![]).pause_on_query(1);
    reader.shutdown = SourceShutdown::Unconfirmed;
    {
        let pending = read(&mut reader, 20);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(1), &mut pending).await.is_err());
    }
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.closed, 0, "the native owner must perform shutdown before admission can reopen");
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
#[ignore = "requires the owned loopback SQL Server provider fixture"]
async fn live_native_memory_clerk_groups_preserve_scope_and_permission_failures() {
    use crate::{conn::Pool, read::ReadSession, test_support::config};
    use std::sync::Arc;
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let pool = Arc::new(Pool::new(config(port, "culprit_smoke", &password, "culprit"), None));
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader, MAX_CLERK_GROUPS).await;
    let samples = reader.finish(result).await.unwrap();
    assert!(!samples.is_empty());
    assert!(samples.iter().any(|sample| sample.identity.id == "MEMORYCLERK_SQLBUFFERPOOL"));
    assert!(samples.iter().all(|sample| sample.is_valid() && sample.scope == MeasurementScope::Server));
    assert!(samples.iter().all(|sample| {
        sample
            .counters
            .iter()
            .find(|counter| counter.key == "clerk_count")
            .and_then(|counter| counter.value.integer())
            .is_some_and(|count| count > 0)
    }));
    let mut configuration = config(port, "culprit_smoke", &password, "culprit");
    configuration.authentication(tiberius::AuthMethod::sql_server(
        std::env::var("CULPRIT_SMOKE_LIMITED_USER").unwrap(),
        std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").unwrap(),
    ));
    let mut reader = ReadSession::new(Arc::new(Pool::new(configuration, None)), CollectionBudget::default());
    let result = read(&mut reader, MAX_CLERK_GROUPS).await;
    assert_eq!(reader.finish(result).await.unwrap_err().code, ErrorCode::PermissionDenied);
}
