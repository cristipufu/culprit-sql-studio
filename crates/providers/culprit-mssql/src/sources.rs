//! Every source Culprit reads on SQL Server, in one list. A scan reads each through [`read`], which is
//! `Coverage::read` for this driver's errors: a source that fails becomes a note, and the capabilities it delivers are
//! locked with why (a permission error says so, with the grant this login needs). A source without capabilities adds
//! detail some checks use; without it those checks say less, never something wrong.

use culprit_core::{Capability, Coverage, ErrorCode, Source, source_id};
pub const SUSPECT_PAGES: Source = Source {
    name: source_id::SUSPECT_PAGES,
    reads: culprit_core::key!("provider.mssql.source.suspect_pages"),
    delivers: &[],
};
pub const LOG_CAPACITY: Source = Source {
    name: source_id::LOG_CAPACITY,
    reads: culprit_core::key!("provider.mssql.source.log_capacity"),
    delivers: &[],
};
pub const FILE_CAPACITY: Source = Source {
    name: source_id::FILE_CAPACITY,
    reads: culprit_core::key!("provider.mssql.source.file_capacity"),
    delivers: &[],
};
pub const TEMP_CAPACITY: Source = Source {
    name: source_id::TEMP_CAPACITY,
    reads: culprit_core::key!("provider.mssql.source.temp_capacity"),
    delivers: &[],
};
pub const SEQUENCE_CAPACITY: Source = Source {
    name: source_id::SEQUENCE_CAPACITY,
    reads: culprit_core::key!("provider.mssql.source.sequence_capacity"),
    delivers: &[],
};

/// Query Store's runtime statistics, plans and waits per query, by interval.
pub const QUERY_STORE: Source = Source {
    name: source_id::QUERY_STORE,
    reads: culprit_core::key!("provider.mssql.source.query_store"),
    delivers: &[Capability::QueryStore],
};
/// Query Store wait categories over the selected workload window, including known zeroes.
pub const QUERY_WAITS: Source = Source {
    name: source_id::QUERY_WAITS,
    reads: culprit_core::key!("provider.mssql.source.query_waits"),
    delivers: &[],
};
/// sys.dm_exec_query_stats: totals per cached plan, where Query Store is off.
pub const PLAN_CACHE: Source = Source {
    name: source_id::PLAN_CACHE,
    reads: culprit_core::key!("provider.mssql.source.plan_cache"),
    delivers: &[Capability::PlanCache],
};
/// Object generations and native creation/alteration dates; no removal history is inferred.
pub const OBJECTS: Source =
    Source { name: source_id::OBJECTS, reads: culprit_core::key!("provider.mssql.source.objects"), delivers: &[] };
pub const TRIGGERS: Source =
    Source { name: source_id::TRIGGERS, reads: culprit_core::key!("provider.mssql.source.triggers"), delivers: &[] };
pub const PARTITIONS: Source = Source {
    name: source_id::PARTITIONS,
    reads: culprit_core::key!("provider.mssql.source.partitions"),
    delivers: &[],
};
pub const RETAINED_PLANS: Source = Source {
    name: source_id::RETAINED_PLANS,
    reads: culprit_core::key!("provider.mssql.source.retained_plans"),
    delivers: &[],
};
/// Row counts, sizes and heap or clustered storage per table.
pub const TABLES: Source = Source {
    name: source_id::TABLES,
    reads: culprit_core::key!("provider.mssql.source.tables"),
    delivers: &[Capability::TableStats],
};
/// Keys, sizes and sys.dm_db_index_usage_stats per index.
pub const INDEXES: Source = Source {
    name: source_id::INDEXES,
    reads: culprit_core::key!("provider.mssql.source.indexes"),
    delivers: &[Capability::IndexUsage],
};
pub const COLUMNS: Source =
    Source { name: source_id::COLUMNS, reads: culprit_core::key!("provider.mssql.source.columns"), delivers: &[] };
pub const FOREIGN_KEYS: Source = Source {
    name: source_id::FOREIGN_KEYS,
    reads: culprit_core::key!("provider.mssql.source.foreign_keys"),
    delivers: &[],
};
/// Statistics objects: when each was updated and how many rows changed since.
pub const STATISTICS: Source = Source {
    name: source_id::STATISTICS,
    reads: culprit_core::key!("provider.mssql.source.statistics"),
    delivers: &[Capability::Statistics],
};
/// The optimizer's missing-index requests.
pub const MISSING_INDEXES: Source = Source {
    name: source_id::MISSING_INDEXES,
    reads: culprit_core::key!("provider.mssql.source.missing_indexes"),
    delivers: &[Capability::MissingIndexes],
};
/// Wait totals: this database's (Azure SQL Database) or the instance's.
pub const WAITS: Source = Source {
    name: source_id::WAITS,
    reads: culprit_core::key!("provider.mssql.source.waits"),
    delivers: &[Capability::WaitStats],
};
/// sys.dm_db_resource_stats on Azure SQL Database: CPU, I/O, log and workers against the tier's limits.
pub const RESOURCES: Source = Source {
    name: source_id::RESOURCES,
    reads: culprit_core::key!("provider.mssql.source.resources"),
    delivers: &[Capability::ResourceUsage],
};
pub const POOL_RESOURCES: Source = Source {
    name: source_id::POOL_RESOURCES,
    reads: culprit_core::key!("provider.mssql.source.pool_resources"),
    delivers: &[],
};
pub const MEMORY_CLERKS: Source = Source {
    name: crate::memory_clerks::SOURCE_NAME,
    reads: culprit_core::key!("provider.mssql.source.memory_clerks"),
    delivers: &[],
};
pub const MEMORY_FAILURES: Source = Source {
    name: source_id::MEMORY_FAILURES,
    reads: culprit_core::key!("provider.mssql.source.memory_failures"),
    delivers: &[],
};
pub const SCHEDULERS: Source = Source {
    name: source_id::SCHEDULERS,
    reads: culprit_core::key!("provider.mssql.source.schedulers"),
    delivers: &[],
};
/// Heaps' forwarded fetches, for the index advisor.
pub const FORWARDED_FETCHES: Source = Source {
    name: source_id::FORWARDED_FETCHES,
    reads: culprit_core::key!("provider.mssql.source.forwarded_fetches"),
    delivers: &[],
};
/// Statements that differ only in their literals.
pub const QUERY_FAMILIES: Source = Source {
    name: source_id::QUERY_FAMILIES,
    reads: culprit_core::key!("provider.mssql.source.query_families"),
    delivers: &[],
};
/// How much of the plan cache single-use plans take.
pub const PLAN_CACHE_USE: Source = Source {
    name: source_id::PLAN_CACHE_USE,
    reads: culprit_core::key!("provider.mssql.source.plan_cache_use"),
    delivers: &[],
};
/// Automatic tuning's recommendations and forced plans.
pub const TUNING: Source =
    Source { name: source_id::TUNING, reads: culprit_core::key!("provider.mssql.source.tuning"), delivers: &[] };
/// Database, scoped and server settings.
pub const SETTINGS: Source =
    Source { name: source_id::SETTINGS, reads: culprit_core::key!("provider.mssql.source.settings"), delivers: &[] };
pub const DATABASE_SETTINGS: Source = Source {
    name: source_id::DATABASE_SETTINGS,
    reads: culprit_core::key!("provider.mssql.source.database_settings"),
    delivers: &[],
};
pub const SCOPED_SETTINGS: Source = Source {
    name: source_id::SCOPED_SETTINGS,
    reads: culprit_core::key!("provider.mssql.source.scoped_settings"),
    delivers: &[],
};
pub const SERVER_SETTINGS: Source = Source {
    name: source_id::SERVER_SETTINGS,
    reads: culprit_core::key!("provider.mssql.source.server_settings"),
    delivers: &[],
};
pub const QUERY_STORE_SETTINGS: Source = Source {
    name: source_id::QUERY_STORE_SETTINGS,
    reads: culprit_core::key!("provider.mssql.source.query_store_settings"),
    delivers: &[],
};
pub const PROCESSOR_TOPOLOGY: Source = Source {
    name: source_id::PROCESSOR_TOPOLOGY,
    reads: culprit_core::key!("provider.mssql.source.processor_topology"),
    delivers: &[],
};
pub const COMPILATION_GATEWAYS: Source = Source {
    name: source_id::COMPILATION_GATEWAYS,
    reads: culprit_core::key!("provider.mssql.source.compilation_gateways"),
    delivers: &[],
};
pub const COMPILATION_QUEUE: Source = Source {
    name: source_id::COMPILATION_QUEUE,
    reads: culprit_core::key!("provider.mssql.source.compilation_queue"),
    delivers: &[],
};
/// Performance counters: batches, compiles, page life.
pub const PERFORMANCE_COUNTERS: Source = Source {
    name: source_id::PERFORMANCE_COUNTERS,
    reads: culprit_core::key!("provider.mssql.source.performance_counters"),
    delivers: &[],
};
/// The last hour's resource use on Azure SQL Database, for the health signals.
pub const RESOURCE_USE: Source = Source {
    name: source_id::RESOURCE_USE,
    reads: culprit_core::key!("provider.mssql.source.resource_use"),
    delivers: &[],
};
/// Queries waiting for a memory grant now.
pub const PENDING_GRANTS: Source = Source {
    name: source_id::PENDING_GRANTS,
    reads: culprit_core::key!("provider.mssql.source.pending_grants"),
    delivers: &[],
};
pub const CURRENT_REQUEST_MEMORY: Source = Source {
    name: source_id::CURRENT_REQUEST_MEMORY,
    reads: culprit_core::key!("provider.mssql.source.current_request_memory"),
    delivers: &[],
};
pub const PAGE_WAIT_TARGETS: Source = Source {
    name: source_id::PAGE_WAIT_TARGETS,
    reads: culprit_core::key!("provider.mssql.source.page_wait_targets"),
    delivers: &[],
};
pub const TASK_SPACE: Source = Source {
    name: source_id::TASK_SPACE,
    reads: culprit_core::key!("provider.mssql.source.task_space"),
    delivers: &[],
};
pub const WORKER_QUEUE: Source = Source {
    name: source_id::WORKER_QUEUE,
    reads: culprit_core::key!("provider.mssql.source.worker_queue"),
    delivers: &[],
};
pub const VERSION_STORE: Source = Source {
    name: source_id::VERSION_STORE,
    reads: culprit_core::key!("provider.mssql.source.version_store"),
    delivers: &[],
};
pub const PERSISTENT_VERSION_STORE: Source = Source {
    name: source_id::PERSISTENT_VERSION_STORE,
    reads: culprit_core::key!("provider.mssql.source.persistent_version_store"),
    delivers: &[],
};

/// Signal, thread pool and memory waits, for the health signals.
pub const WAIT_TOTALS: Source = Source {
    name: source_id::WAIT_TOTALS,
    reads: culprit_core::key!("provider.mssql.source.wait_totals"),
    delivers: &[],
};
pub const TUNING_OPTIONS: Source = Source {
    name: source_id::TUNING_OPTIONS,
    reads: culprit_core::key!("provider.mssql.source.tuning_options"),
    delivers: &[],
};
pub const TUNING_RECOMMENDATIONS: Source = Source {
    name: source_id::TUNING_RECOMMENDATIONS,
    reads: culprit_core::key!("provider.mssql.source.tuning_recommendations"),
    delivers: &[],
};
pub const FORCED_PLANS: Source = Source {
    name: source_id::FORCED_PLANS,
    reads: culprit_core::key!("provider.mssql.source.forced_plans"),
    delivers: &[],
};
pub const QUERY_HINTS: Source = Source {
    name: source_id::QUERY_HINTS,
    reads: culprit_core::key!("provider.mssql.source.query_hints"),
    delivers: &[],
};
/// Memory granted and used per cached query.
pub const GRANTS: Source =
    Source { name: source_id::GRANTS, reads: culprit_core::key!("provider.mssql.source.grants"), delivers: &[] };
/// The oldest open transaction, and what it holds back.
pub const OPEN_TRANSACTIONS: Source = Source {
    name: source_id::OPEN_TRANSACTIONS,
    reads: culprit_core::key!("provider.mssql.source.open_transactions"),
    delivers: &[],
};

/// CPU, reads and waits per query beyond time (Query Store's runtime statistics).
pub const QUERY_COSTS: Source = Source {
    name: source_id::QUERY_COSTS,
    reads: culprit_core::key!("provider.mssql.source.query_costs"),
    delivers: &[],
};
/// This database's compilation counts and timings for retained Query Store entries.
pub const QUERY_COMPILATION: Source = Source {
    name: source_id::QUERY_COMPILATION,
    reads: culprit_core::key!("provider.mssql.source.query_compilation"),
    delivers: &[],
};
/// Row and page lock waits, latch waits and escalations per index.
pub const INDEX_OPERATIONS: Source = Source {
    name: source_id::INDEX_OPERATIONS,
    reads: culprit_core::key!("provider.mssql.source.index_operations"),
    delivers: &[],
};

pub const INDEX_CONTENTION: Source = Source {
    name: source_id::INDEX_CONTENTION,
    reads: culprit_core::key!("provider.mssql.source.index_contention"),
    delivers: &[],
};
/// Foreign keys and CHECK constraints not trusted.
pub const CONSTRAINTS: Source = Source {
    name: source_id::CONSTRAINTS,
    reads: culprit_core::key!("provider.mssql.source.constraints"),
    delivers: &[],
};
pub const CHECK_CONSTRAINTS: Source = Source {
    name: source_id::CHECK_CONSTRAINTS,
    reads: culprit_core::key!("provider.mssql.source.check_constraints"),
    delivers: &[],
};
/// sys.dm_io_virtual_file_stats: read and write latency per file.
pub const FILE_LATENCY: Source = Source {
    name: source_id::FILE_LATENCY,
    reads: culprit_core::key!("provider.mssql.source.file_latency"),
    delivers: &[],
};
/// Logins and connection resets, for connection churn.
pub const LOGINS: Source =
    Source { name: source_id::LOGINS, reads: culprit_core::key!("provider.mssql.source.logins"), delivers: &[] };
pub const REPLICATION: Source = Source {
    name: source_id::REPLICATION,
    reads: culprit_core::key!("provider.mssql.source.replication"),
    delivers: &[],
};

pub const SCHEMA: &[Source] = &[TABLES, INDEXES, COLUMNS, FOREIGN_KEYS, STATISTICS, CONSTRAINTS, MISSING_INDEXES];
pub const WORKLOAD: &[Source] = &[QUERY_STORE, QUERY_WAITS, PLAN_CACHE];
pub const MISC: &[Source] = &[
    SETTINGS,
    DATABASE_SETTINGS,
    SCOPED_SETTINGS,
    SERVER_SETTINGS,
    QUERY_STORE_SETTINGS,
    WAITS,
    QUERY_FAMILIES,
    PLAN_CACHE_USE,
    TUNING,
    TUNING_OPTIONS,
    TUNING_RECOMMENDATIONS,
    FORCED_PLANS,
    QUERY_HINTS,
    PROCESSOR_TOPOLOGY,
    COMPILATION_GATEWAYS,
    COMPILATION_QUEUE,
    PERFORMANCE_COUNTERS,
    LOGINS,
    PENDING_GRANTS,
    WORKER_QUEUE,
    VERSION_STORE,
    PERSISTENT_VERSION_STORE,
    WAIT_TOTALS,
    GRANTS,
    OPEN_TRANSACTIONS,
];
pub const SIGNAL_READS: &[Source] = &[
    REPLICATION,
    QUERY_COSTS,
    QUERY_COMPILATION,
    INDEX_CONTENTION,
    FORWARDED_FETCHES,
    FILE_LATENCY,
    LOG_CAPACITY,
    FILE_CAPACITY,
    TEMP_CAPACITY,
    SEQUENCE_CAPACITY,
    SUSPECT_PAGES,
];

/// Every source above, for listings (`culprit sources`).
pub const ALL: &[Source] = &[
    MEMORY_FAILURES,
    CURRENT_REQUEST_MEMORY,
    PAGE_WAIT_TARGETS,
    TASK_SPACE,
    REPLICATION,
    LOG_CAPACITY,
    FILE_CAPACITY,
    TEMP_CAPACITY,
    SEQUENCE_CAPACITY,
    SUSPECT_PAGES,
    QUERY_STORE,
    QUERY_WAITS,
    PLAN_CACHE,
    TABLES,
    INDEXES,
    COLUMNS,
    FOREIGN_KEYS,
    STATISTICS,
    CHECK_CONSTRAINTS,
    OBJECTS,
    TRIGGERS,
    PARTITIONS,
    RETAINED_PLANS,
    MISSING_INDEXES,
    WAITS,
    RESOURCES,
    POOL_RESOURCES,
    MEMORY_CLERKS,
    SCHEDULERS,
    FORWARDED_FETCHES,
    QUERY_FAMILIES,
    PLAN_CACHE_USE,
    TUNING,
    TUNING_OPTIONS,
    TUNING_RECOMMENDATIONS,
    FORCED_PLANS,
    QUERY_HINTS,
    SETTINGS,
    DATABASE_SETTINGS,
    SCOPED_SETTINGS,
    SERVER_SETTINGS,
    QUERY_STORE_SETTINGS,
    PROCESSOR_TOPOLOGY,
    COMPILATION_GATEWAYS,
    COMPILATION_QUEUE,
    PERFORMANCE_COUNTERS,
    RESOURCE_USE,
    PENDING_GRANTS,
    WORKER_QUEUE,
    VERSION_STORE,
    PERSISTENT_VERSION_STORE,
    WAIT_TOTALS,
    GRANTS,
    OPEN_TRANSACTIONS,
    QUERY_COSTS,
    QUERY_COMPILATION,
    INDEX_CONTENTION,
    INDEX_OPERATIONS,
    CONSTRAINTS,
    FILE_LATENCY,
    LOGINS,
];

/// Record a bounded native read, adding this database's permission hint when applicable.
pub(crate) fn read<T>(
    coverage: &mut Coverage,
    source: &Source,
    result: culprit_core::Result<T>,
    grant: impl FnOnce() -> String,
) -> Option<T> {
    coverage.read(
        source,
        result.map_err(|error| {
            if error.code == ErrorCode::PermissionDenied && error.hint.is_none() {
                error.with_hint(grant())
            } else {
                error
            }
        }),
    )
}

/// A catalog read also records whether it returned any rows.
pub(crate) fn read_rows<T>(
    coverage: &mut Coverage,
    source: &Source,
    result: culprit_core::Result<Vec<T>>,
    grant: impl FnOnce() -> String,
) -> Option<Vec<T>> {
    let rows = read(coverage, source, result, grant)?;
    coverage.completed(source, rows.len());
    Some(rows)
}
