use culprit_core::{Capability, Source, source_id};
pub const TABLES: Source = Source {
    name: source_id::TABLES,
    reads: culprit_core::key!("provider.yugabyte.source.tables"),
    delivers: &[Capability::TableStats, Capability::Statistics],
};
pub const INDEXES: Source =
    Source { name: source_id::INDEXES, reads: culprit_core::key!("provider.yugabyte.source.indexes"), delivers: &[] };
pub const SIZES: Source =
    Source { name: source_id::SIZES, reads: culprit_core::key!("provider.yugabyte.source.sizes"), delivers: &[] };
pub const STORAGE: Source = Source {
    name: source_id::TABLE_STORAGE,
    reads: culprit_core::key!("provider.yugabyte.source.storage"),
    delivers: &[],
};
pub const COLUMNS: Source =
    Source { name: source_id::COLUMNS, reads: culprit_core::key!("provider.yugabyte.source.columns"), delivers: &[] };
pub const FOREIGN_KEYS: Source = Source {
    name: source_id::FOREIGN_KEYS,
    reads: culprit_core::key!("provider.yugabyte.source.foreign_keys"),
    delivers: &[],
};
pub const CONSTRAINTS: Source = Source {
    name: source_id::CONSTRAINTS,
    reads: culprit_core::key!("provider.yugabyte.source.constraints"),
    delivers: &[],
};
pub const WORKLOAD: Source = Source {
    name: source_id::WORKLOAD,
    reads: culprit_core::key!("provider.yugabyte.source.workload"),
    delivers: &[Capability::PgStatStatements],
};
pub const COSTS: Source =
    Source { name: source_id::QUERY_COSTS, reads: culprit_core::key!("provider.yugabyte.source.costs"), delivers: &[] };
pub const PLAN_HISTORY: Source = Source {
    name: source_id::DOCDB_PLAN_HISTORY,
    reads: culprit_core::key!("provider.yugabyte.source.plan_history"),
    delivers: &[],
};
pub const ACTIVITY_HISTORY: Source = Source {
    name: source_id::DOCDB_ACTIVITY_HISTORY,
    reads: culprit_core::key!("provider.yugabyte.source.activity_history"),
    delivers: &[],
};
pub const TERMINATIONS: Source = Source {
    name: source_id::DOCDB_TERMINATIONS,
    reads: culprit_core::key!("provider.yugabyte.source.terminations"),
    delivers: &[],
};
pub const NODE_RESOURCES: Source = Source {
    name: source_id::DOCDB_NODE_RESOURCES,
    reads: culprit_core::key!("provider.yugabyte.source.node_resources"),
    delivers: &[],
};
pub const EPOCHS: Source = Source {
    name: source_id::DOCDB_EPOCHS,
    reads: culprit_core::key!("provider.yugabyte.source.epochs"),
    delivers: &[],
};
pub const SETTINGS: Source =
    Source { name: source_id::SETTINGS, reads: culprit_core::key!("provider.yugabyte.source.settings"), delivers: &[] };
pub const INDEX_USAGE: Source = Source {
    name: source_id::DOCDB_INDEX_USAGE,
    reads: culprit_core::key!("provider.yugabyte.source.index_usage"),
    delivers: &[Capability::IndexUsage],
};
pub const ALL: &[Source] = &[
    TABLES,
    INDEXES,
    SIZES,
    STORAGE,
    COLUMNS,
    FOREIGN_KEYS,
    CONSTRAINTS,
    WORKLOAD,
    COSTS,
    PLAN_HISTORY,
    ACTIVITY_HISTORY,
    TERMINATIONS,
    NODE_RESOURCES,
    EPOCHS,
    SETTINGS,
    INDEX_USAGE,
];
