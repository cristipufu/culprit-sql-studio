//! Every source Culprit reads on PostgreSQL, in one list. A scan reads each through `Coverage::read`: a source that
//! fails becomes a note, and the capabilities it delivers are locked with why. A source without capabilities adds
//! detail some checks use (a table's HOT updates, a replica's lag); without it those checks say less, never something
//! wrong.

use culprit_core::{Capability, Source, source_id};
pub const MULTIXACTS: Source = Source {
    name: source_id::MULTIXACTS,
    reads: culprit_core::key!("provider.postgres.source.multixacts"),
    delivers: &[],
};
pub const ARCHIVER: Source =
    Source { name: source_id::ARCHIVER, reads: culprit_core::key!("provider.postgres.source.archiver"), delivers: &[] };
pub const SUBSCRIPTION_ERRORS: Source = Source {
    name: source_id::SUBSCRIPTION_ERRORS,
    reads: culprit_core::key!("provider.postgres.source.subscription_errors"),
    delivers: &[],
};
pub const STANDBY_CONFLICTS: Source = Source {
    name: source_id::STANDBY_CONFLICTS,
    reads: culprit_core::key!("provider.postgres.source.standby_conflicts"),
    delivers: &[],
};
pub const SEQUENCE_CAPACITY: Source = Source {
    name: source_id::SEQUENCE_CAPACITY,
    reads: culprit_core::key!("provider.postgres.source.sequence_capacity"),
    delivers: &[],
};

/// Statement statistics: runs, time, rows and buffers per normalized statement.
pub const STATEMENTS: Source = Source {
    name: source_id::WORKLOAD,
    reads: culprit_core::key!("provider.postgres.source.statements"),
    delivers: &[Capability::PgStatStatements],
};
/// pg_stat_user_tables and the catalog's sizes: rows, scans, writes, dead rows, analyze and vacuum times.
pub const TABLES: Source = Source {
    name: source_id::TABLES,
    reads: culprit_core::key!("provider.postgres.source.tables"),
    delivers: &[Capability::TableStats, Capability::Statistics],
};
/// pg_index with pg_stat_user_indexes: keys, kinds, sizes and reads per index.
pub const INDEXES: Source = Source {
    name: source_id::INDEXES,
    reads: culprit_core::key!("provider.postgres.source.indexes"),
    delivers: &[Capability::IndexUsage],
};
pub const COLUMNS: Source =
    Source { name: source_id::COLUMNS, reads: culprit_core::key!("provider.postgres.source.columns"), delivers: &[] };
pub const FOREIGN_KEYS: Source = Source {
    name: source_id::FOREIGN_KEYS,
    reads: culprit_core::key!("provider.postgres.source.foreign_keys"),
    delivers: &[],
};
/// pg_settings: the settings checks read, and what changed them.
pub const SETTINGS: Source =
    Source { name: source_id::SETTINGS, reads: culprit_core::key!("provider.postgres.source.settings"), delivers: &[] };
/// pg_stat_database: cache hits, temp files, commits and rollbacks, deadlocks, and how long they have counted.
pub const DATABASE_STATS: Source = Source {
    name: source_id::DATABASE_STATS,
    reads: culprit_core::key!("provider.postgres.source.database_stats"),
    delivers: &[],
};
/// pg_stat_checkpointer (17+) or pg_stat_bgwriter: checkpoints timed and requested.
pub const CHECKPOINTS: Source = Source {
    name: source_id::CHECKPOINTS,
    reads: culprit_core::key!("provider.postgres.source.checkpoints"),
    delivers: &[],
};
/// pg_stat_activity: the oldest transaction holding cleanup back.
pub const OPEN_TRANSACTIONS: Source = Source {
    name: source_id::OPEN_TRANSACTIONS,
    reads: culprit_core::key!("provider.postgres.source.open_transactions"),
    delivers: &[],
};
pub const CLEANUP_SLOTS: Source = Source {
    name: source_id::CLEANUP_SLOTS,
    reads: culprit_core::key!("provider.postgres.source.cleanup_slots"),
    delivers: &[],
};
pub const CLEANUP_PREPARED: Source = Source {
    name: source_id::CLEANUP_PREPARED,
    reads: culprit_core::key!("provider.postgres.source.cleanup_prepared"),
    delivers: &[],
};
/// Planning, JIT and WAL per statement (pg_stat_statements' newer columns).
pub const QUERY_COSTS: Source = Source {
    name: source_id::QUERY_COSTS,
    reads: culprit_core::key!("provider.postgres.source.query_costs"),
    delivers: &[],
};
/// HOT and new-page updates, fillfactor and the bloat estimate per table.
pub const TABLE_STORAGE: Source = Source {
    name: source_id::TABLE_STORAGE,
    reads: culprit_core::key!("provider.postgres.source.table_storage"),
    delivers: &[],
};
/// CHECK constraints added NOT VALID.
pub const CONSTRAINTS: Source = Source {
    name: source_id::CONSTRAINTS,
    reads: culprit_core::key!("provider.postgres.source.constraints"),
    delivers: &[],
};
/// The settings advice depends on (wal_compression, constraint_exclusion, max_slot_wal_keep_size).
pub const ADVICE_SETTINGS: Source = Source {
    name: source_id::ADVICE_SETTINGS,
    reads: culprit_core::key!("provider.postgres.source.advice_settings"),
    delivers: &[],
};
/// pg_stat_io, pg_stat_database and pg_stat_wal: read and write latency.
pub const FILE_LATENCY: Source = Source {
    name: source_id::FILE_LATENCY,
    reads: culprit_core::key!("provider.postgres.source.file_latency"),
    delivers: &[],
};
pub const SHARED_MEMORY_ALLOCATIONS: Source = Source {
    name: source_id::SHARED_MEMORY_ALLOCATIONS,
    reads: culprit_core::key!("provider.postgres.source.shared_memory_allocations"),
    delivers: &[],
};
/// Sessions opened and abandoned (14+).
pub const SESSIONS: Source =
    Source { name: source_id::SESSIONS, reads: culprit_core::key!("provider.postgres.source.sessions"), delivers: &[] };
/// Replicas, this server's own lag as a standby, and replication slots (10+).
pub const REPLICATION: Source = Source {
    name: source_id::REPLICATION,
    reads: culprit_core::key!("provider.postgres.source.replication"),
    delivers: &[],
};

pub const HYPERTABLES: Source = Source {
    name: source_id::TIMESCALE_HYPERTABLES,
    reads: culprit_core::key!("provider.postgres.source.hypertables"),
    delivers: &[Capability::TableStats, Capability::Statistics, Capability::IndexUsage],
};
pub const CHUNKS: Source = Source {
    name: source_id::TIMESCALE_CHUNKS,
    reads: culprit_core::key!("provider.postgres.source.chunks"),
    delivers: &[],
};
pub const HYPERTABLE_INDEXES: Source = Source {
    name: source_id::TIMESCALE_INDEXES,
    reads: culprit_core::key!("provider.postgres.source.hypertable_indexes"),
    delivers: &[],
};

/// Sources sharing the auxiliary connection. A session failure accounts for all of them.
pub const MISC: &[Source] =
    &[SETTINGS, DATABASE_STATS, CHECKPOINTS, OPEN_TRANSACTIONS, CLEANUP_SLOTS, CLEANUP_PREPARED];

/// Every source above, for listings (`culprit sources`).
pub const ALL: &[Source] = &[
    HYPERTABLES,
    CHUNKS,
    HYPERTABLE_INDEXES,
    MULTIXACTS,
    ARCHIVER,
    SUBSCRIPTION_ERRORS,
    STANDBY_CONFLICTS,
    SEQUENCE_CAPACITY,
    STATEMENTS,
    TABLES,
    INDEXES,
    COLUMNS,
    FOREIGN_KEYS,
    SETTINGS,
    DATABASE_STATS,
    CHECKPOINTS,
    OPEN_TRANSACTIONS,
    CLEANUP_SLOTS,
    CLEANUP_PREPARED,
    QUERY_COSTS,
    TABLE_STORAGE,
    CONSTRAINTS,
    ADVICE_SETTINGS,
    FILE_LATENCY,
    SHARED_MEMORY_ALLOCATIONS,
    SESSIONS,
    REPLICATION,
];
