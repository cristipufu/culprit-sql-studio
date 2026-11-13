use super::super::metadata::StatementCatalog;
use super::*;

fn metadata(version: u32, extension_info: bool) -> Metadata {
    Metadata {
        database: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Postgres,
            "fixture",
            "fixture.invalid",
            5432,
            None,
        ),
        database_oid: "42".into(),
        version,
        started_at: "2020-01-01T05:00:00Z".into(),
        at: "2020-01-01T06:00:00Z".into(),
        block_bytes: 8192,
        full_visibility: true,
        standby: false,
        statements: extension_info.then(|| StatementCatalog {
            schema: "diagnostic".into(),
            columns: Default::default(),
            info: true,
        }),
    }
}

#[test]
fn native_version_and_ownership_select_one_disjoint_source_set() {
    for version in [100_000, 130_000, 140_000, 150_000, 160_000, 170_000, 180_000] {
        let meta = metadata(version, true);
        let database = selected_sources(&meta, Selection::Database);
        let instance = selected_sources(&meta, Selection::Instance);
        assert!(!database.is_empty() && !instance.is_empty());
        assert!(database.iter().all(|source| !instance.contains(source)));
        let all = sources(&meta);
        assert_eq!(database.len() + instance.len(), all.len());
        assert!(all.iter().all(|source| database.contains(source) || instance.contains(source)));
        assert!(database.contains(&"pg_stat_database") && database.contains(&"pg_database"));
        assert!(
            instance.contains(&"pg_stat_statements_info"),
            "the extension's reset/deallocation totals cover all databases"
        );
        assert_eq!(instance.contains(&"pg_stat_wal"), version >= 140_000);
        assert_eq!(database.contains(&"pg_stat_subscription_stats"), version >= 150_000);
        assert_eq!(instance.contains(&"pg_stat_io"), version >= 160_000);
        assert_eq!(instance.contains(&"pg_stat_checkpointer"), version >= 170_000);
        assert_eq!(instance.contains(&super::super::memory::SOURCE_NAME), version >= 130_000);
        let database_sql = selected_query(&meta, 51, Selection::Database, crate::native::memory::Access::Available);
        let instance_sql = selected_query(&meta, 51, Selection::Instance, crate::native::memory::Access::Available);
        assert!(database_sql.contains("WHERE s.datid=42") && database_sql.ends_with("LIMIT 51"));
        assert!(!database_sql.contains("pg_stat_bgwriter") && !database_sql.contains("pg_stat_statements_info"));
        assert!(!instance_sql.contains("WHERE s.datid=") && !instance_sql.contains("pg_stat_database"));
        assert!(instance_sql.contains("diagnostic.pg_stat_statements_info"));
    }
    assert!(!selected_sources(&metadata(180_000, false), Selection::Instance).contains(&"pg_stat_statements_info"));
}

#[test]
fn denied_shared_memory_remains_declared_but_does_not_remove_other_instance_sources() {
    let meta = metadata(180_000, true);
    let sources = selected_sources(&meta, Selection::Instance);
    assert!(sources.contains(&super::super::memory::SOURCE_NAME));
    let query = selected_query(&meta, 51, Selection::Instance, super::super::memory::Access::Restricted);
    assert!(!query.contains("pg_shmem_allocations"));
    assert!(query.contains("pg_catalog.pg_stat_bgwriter") && query.contains("pg_catalog.pg_stat_wal"));
    let memory = serde_json::json!({"name":"Buffer Blocks","off":"4096","size":"8192","allocated_size":"8192"});
    let row = Row::from_fields([
        ("native_source".into(), Some(super::super::memory::SOURCE_NAME.into())),
        ("native_values".into(), Some(memory.to_string())),
    ]);
    let TelemetryRecord::Operational(observation) = decode(&row, &meta, Selection::Instance).unwrap() else { panic!() };
    assert_eq!(observation.source, super::super::memory::SOURCE_NAME);
    let OperationalFact::CounterSet(counters) = observation.fact else { panic!() };
    assert_eq!(counters.properties["name"], "Buffer Blocks");
    assert_eq!(counters.counters[0].value.integer(), Some(8192));
    assert!(decode(&row, &meta, Selection::Database).is_err());
}

fn row(source: &str, values: serde_json::Value) -> Row {
    Row::from_fields([
        ("native_source".into(), Some(source.into())),
        ("native_values".into(), Some(values.to_string())),
    ])
}

#[test]
fn native_decoding_enforces_requested_scope_before_admitting_counters() {
    let meta = metadata(180_000, true);
    let database = row(
        "pg_stat_database",
        serde_json::json!({"datname":"fixture","deadlocks":0,"stats_reset":"2020-01-01T05:30:00Z"}),
    );
    let instance = row(
        "pg_stat_archiver",
        serde_json::json!({"archived_count":42,"failed_count":null,"stats_reset":"2020-01-01T05:30:00Z"}),
    );
    assert!(decode(&database, &meta, Selection::Instance).is_err());
    assert!(decode(&instance, &meta, Selection::Database).is_err());
    let TelemetryRecord::Operational(observation) = decode(&database, &meta, Selection::Database).unwrap() else {
        panic!()
    };
    let OperationalFact::CounterSet(counters) = observation.fact else { panic!() };
    assert_eq!(counters.scope, MeasurementScope::Database);
    assert_eq!(counters.identity.qualifiers.get("dbid").map(String::as_str), Some("42"));
    assert_eq!(counters.counters[0].value.integer(), Some(0));
    let TelemetryRecord::Operational(observation) = decode(&instance, &meta, Selection::Instance).unwrap() else {
        panic!()
    };
    let OperationalFact::CounterSet(counters) = observation.fact else { panic!() };
    assert_eq!(counters.scope, MeasurementScope::Server);
    assert!(!counters.identity.qualifiers.contains_key("dbid"));
    assert_eq!(counters.counters.len(), 1, "NULL failed_count remains unknown, not a measured zero");
    assert_eq!(counters.counters[0].value.integer(), Some(42));
    assert!(counters.counters.iter().all(|counter| counter.scope == MeasurementScope::Server));
    let removed_source = row("pg_stat_checkpointer", serde_json::json!({"num_done":1}));
    assert!(decode(&removed_source, &metadata(160_000, false), Selection::Instance).is_err());
}
