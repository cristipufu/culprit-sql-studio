//! Native progress populations retain their own units and command semantics.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MaintenanceCommand {
    CreateIndex,
    CreateIndexConcurrently,
    Reindex,
    ReindexConcurrently,
    Vacuum,
    Analyze,
    Cluster,
    VacuumFull,
}

impl MaintenanceCommand {
    /// Closed native command values, independently of localized prose or arbitrary SQL text.
    pub fn from_native(value: &str) -> Option<Self> {
        match value {
            "CREATE INDEX" => Some(Self::CreateIndex),
            "CREATE INDEX CONCURRENTLY" => Some(Self::CreateIndexConcurrently),
            "REINDEX" => Some(Self::Reindex),
            "REINDEX CONCURRENTLY" => Some(Self::ReindexConcurrently),
            "VACUUM" => Some(Self::Vacuum),
            "ANALYZE" => Some(Self::Analyze),
            "CLUSTER" => Some(Self::Cluster),
            "VACUUM FULL" => Some(Self::VacuumFull),
            _ => None,
        }
    }

    pub fn as_native(self) -> &'static str {
        match self {
            Self::CreateIndex => "CREATE INDEX",
            Self::CreateIndexConcurrently => "CREATE INDEX CONCURRENTLY",
            Self::Reindex => "REINDEX",
            Self::ReindexConcurrently => "REINDEX CONCURRENTLY",
            Self::Vacuum => "VACUUM",
            Self::Analyze => "ANALYZE",
            Self::Cluster => "CLUSTER",
            Self::VacuumFull => "VACUUM FULL",
        }
    }

    fn is_index_build(self) -> bool {
        matches!(self, Self::CreateIndex | Self::CreateIndexConcurrently | Self::Reindex | Self::ReindexConcurrently)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresIndexBuildProgress {
    pub blocks_total: Option<String>,
    pub blocks_done: Option<String>,
    pub tuples_total: Option<String>,
    pub tuples_done: Option<String>,
    pub lockers_total: Option<String>,
    pub lockers_done: Option<String>,
    /// A raw PID cannot establish another session's generation or SQL.
    pub current_locker_pid: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct YsqlIndexBuildProgress {
    pub tuples_done: Option<String>,
    /// DocDB's backfill estimate is separate from PostgreSQL heap work and whole-command completion.
    pub tuples_total_estimate: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresVacuumProgress {
    pub heap_blks_total: Option<String>,
    pub heap_blks_scanned: Option<String>,
    pub heap_blks_vacuumed: Option<String>,
    pub index_vacuum_count: Option<String>,
    /// Legacy tuple counts and newer byte/item counters describe different native populations.
    pub max_dead_tuples: Option<String>,
    pub num_dead_tuples: Option<String>,
    pub indexes_total: Option<String>,
    pub indexes_processed: Option<String>,
    pub max_dead_tuple_bytes: Option<String>,
    pub dead_tuple_bytes: Option<String>,
    pub num_dead_item_ids: Option<String>,
    /// Native float8 milliseconds, not an exact integer or complete execution delay. Zero can be untracked.
    pub cost_delay_ms: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresAnalyzeProgress {
    pub sample_blks_total: Option<String>,
    pub sample_blks_scanned: Option<String>,
    pub ext_stats_total: Option<String>,
    pub ext_stats_computed: Option<String>,
    pub child_tables_total: Option<String>,
    pub child_tables_done: Option<String>,
    pub current_child_table_relid: Option<String>,
    pub cost_delay_ms: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresClusterProgress {
    pub heap_tuples_scanned: Option<String>,
    pub heap_tuples_written: Option<String>,
    pub heap_blks_total: Option<String>,
    pub heap_blks_scanned: Option<String>,
    pub index_rebuild_count: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MaintenanceProgressWork {
    PostgreSqlIndexBuild { progress: Box<PostgresIndexBuildProgress> },
    YsqlIndexBuild { progress: Box<YsqlIndexBuildProgress> },
    PostgreSqlVacuum { progress: Box<PostgresVacuumProgress> },
    PostgreSqlAnalyze { progress: Box<PostgresAnalyzeProgress> },
    PostgreSqlCluster { progress: Box<PostgresClusterProgress> },
}

impl MaintenanceProgressWork {
    pub fn engine(&self) -> crate::Engine {
        match self {
            Self::PostgreSqlIndexBuild { .. }
            | Self::PostgreSqlVacuum { .. }
            | Self::PostgreSqlAnalyze { .. }
            | Self::PostgreSqlCluster { .. } => crate::Engine::Postgres,
            Self::YsqlIndexBuild { .. } => crate::Engine::Yugabyte,
        }
    }

    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::PostgreSqlIndexBuild { .. } => "postgres-index-build",
            Self::YsqlIndexBuild { .. } => "ysql-index-build",
            Self::PostgreSqlVacuum { .. } => "postgres-vacuum",
            Self::PostgreSqlAnalyze { .. } => "postgres-analyze",
            Self::PostgreSqlCluster { .. } => "postgres-cluster",
        }
    }

    pub(super) fn accepts_command(&self, command: MaintenanceCommand) -> bool {
        match self {
            Self::PostgreSqlIndexBuild { .. } | Self::YsqlIndexBuild { .. } => command.is_index_build(),
            Self::PostgreSqlVacuum { .. } => command == MaintenanceCommand::Vacuum,
            Self::PostgreSqlAnalyze { .. } => command == MaintenanceCommand::Analyze,
            Self::PostgreSqlCluster { .. } => {
                matches!(command, MaintenanceCommand::Cluster | MaintenanceCommand::VacuumFull)
            }
        }
    }

    pub(super) fn has_index_reference(&self) -> bool {
        matches!(self, Self::PostgreSqlIndexBuild { .. } | Self::YsqlIndexBuild { .. } | Self::PostgreSqlCluster { .. })
    }

    pub fn is_valid(&self) -> bool {
        let counters = match self {
            Self::PostgreSqlIndexBuild { progress: p } => vec![
                &p.blocks_total,
                &p.blocks_done,
                &p.tuples_total,
                &p.tuples_done,
                &p.lockers_total,
                &p.lockers_done,
                &p.current_locker_pid,
            ],
            Self::YsqlIndexBuild { progress: p } => vec![&p.tuples_done, &p.tuples_total_estimate],
            Self::PostgreSqlVacuum { progress: p } => {
                if !p.cost_delay_ms.as_deref().is_none_or(native_milliseconds) {
                    return false;
                }
                vec![
                    &p.heap_blks_total,
                    &p.heap_blks_scanned,
                    &p.heap_blks_vacuumed,
                    &p.index_vacuum_count,
                    &p.max_dead_tuples,
                    &p.num_dead_tuples,
                    &p.indexes_total,
                    &p.indexes_processed,
                    &p.max_dead_tuple_bytes,
                    &p.dead_tuple_bytes,
                    &p.num_dead_item_ids,
                ]
            }
            Self::PostgreSqlAnalyze { progress: p } => {
                if !p.current_child_table_relid.as_deref().is_none_or(oid)
                    || !p.cost_delay_ms.as_deref().is_none_or(native_milliseconds)
                {
                    return false;
                }
                vec![
                    &p.sample_blks_total,
                    &p.sample_blks_scanned,
                    &p.ext_stats_total,
                    &p.ext_stats_computed,
                    &p.child_tables_total,
                    &p.child_tables_done,
                ]
            }
            Self::PostgreSqlCluster { progress: p } => vec![
                &p.heap_tuples_scanned,
                &p.heap_tuples_written,
                &p.heap_blks_total,
                &p.heap_blks_scanned,
                &p.index_rebuild_count,
            ],
        };
        counters.into_iter().flatten().all(|value| canonical_counter(value))
    }
}

fn native_milliseconds(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.parse::<f64>().is_ok_and(|number| number.is_finite() && number >= 0.0)
}
