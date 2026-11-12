//! The facts plan calculations read: recorded by each engine's parser from its native plan, refined by preparation,
//! and stored with the plan. The engine's display properties beside them are for reading only.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct PlanFacts {
    /// Protection confirmed by the native provider for this measured execution. An imported or retained plan cannot
    /// establish how Culprit executed it, and therefore leaves this unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub execution_protection: Option<ExecutionProtection>,
    // The source removed constants from the stored plan; selectivity cannot be justified by those hidden values.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(as = "Option<bool>", optional)]
    pub constants_redacted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(as = "Option<bool>", optional)]
    pub estimated_cost_unavailable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub elapsed_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub cpu_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub planning_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub compile_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub compile_cpu_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub compile_memory_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub memory_desired_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub memory_requested_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub memory_granted_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub memory_max_used_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub memory_grant_wait_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub udf_elapsed_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub udf_cpu_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub degree_of_parallelism: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub used_threads: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub cardinality_estimator: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub cost_threshold_for_parallelism: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub server_maxdop: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub database_maxdop: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub jit_time_ms: Option<f64>,
    #[serde(default, with = "trigger_measurements")]
    #[ts(as = "Vec<crate::actual::TriggerTiming>")]
    pub trigger_timings: Vec<crate::actual::TriggerTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism_blocker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization_early_abort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_plan_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics_maintenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_parameters: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_grant_warning: Option<String>,
    pub conversions: Vec<PlanConversion>,
    pub changed_columns: Vec<PlanChangedColumn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>", optional)]
    pub sample_limit_rows: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub virtual_indexes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub docdb: Option<crate::signals::DocdbMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sample_source: Option<PlanSampleSource>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ExecutionProtection {
    ReadOnlyTransaction,
    RolledBackTransaction,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct PlanNodeFacts {
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(as = "Option<bool>", optional)]
    pub estimated_rows_unavailable: bool,
    // Number of threads whose execution counts contribute to this operator's total, not requested parallelism.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub parallel_threads: Option<u32>,
    // Statement measurements attributable to this scan, from preparation. A fraction in 0..=1, without rounding.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "fraction_number")]
    #[ts(as = "Option<f64>")]
    pub measured_kept_share: Option<f64>,
    // MySQL/MariaDB's own estimate, in percent. A fixed optimizer guess remains distinguishable from a measurement.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "percentage_number")]
    #[ts(as = "Option<f64>")]
    pub estimated_filtered_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selectivity_guess: Option<String>,
    // Why a listed partition may not be read by an execution. Its catalog size cannot establish rows read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_pruning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_parent: Option<String>,
    // The catalog's native table kind. Managed storage can require different DDL from ordinary engine tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub table_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_relationship: Option<PlanParentRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_execution_mode: Option<PlanExecutionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_execution_mode: Option<PlanExecutionMode>,
    // PostgreSQL's original per-process estimate, before Culprit normalizes the parallel subtree.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub estimated_rows_per_process: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub disk_usage_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub sort_space_kb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub hash_batches: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub hash_aggregate_batches: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub heap_fetches: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub lossy_heap_blocks: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub exact_heap_blocks: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub rows_removed_by_recheck: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub estimated_rows_without_goal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub workers_planned: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub workers_launched: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "count_number")]
    #[ts(as = "Option<u32>")]
    pub sort_merge_passes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_on_disk: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_spill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_buffer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_buffer_join_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subplan_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_missing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub many_to_many: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anti_join: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cacheable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub functions_in_seek_only: Option<bool>,
    pub expression_cache: bool,
    pub conditions: Vec<String>,
    pub conversions: Vec<PlanConversion>,
    pub user_functions: Vec<String>,
    pub columns_without_statistics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub used_key_parts: Vec<String>,
    pub possible_index_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub possible_index_columns: Option<Vec<PlanIndexColumns>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_indexes: Option<Vec<PlanIndexColumns>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_index: Option<PlanAutomaticIndex>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub estimated_row_bytes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nonnegative_number")]
    #[ts(as = "Option<f64>")]
    pub rows_produced_per_join: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_columns: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub docdb: Option<crate::signals::DocdbMetrics>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanParentRelationship {
    Outer,
    Inner,
    SubPlan,
    InitPlan,
    Cte,
    Other(String),
}
impl PlanParentRelationship {
    pub fn from_source(value: &str) -> Self {
        match value {
            "Outer" => Self::Outer,
            "Inner" => Self::Inner,
            "SubPlan" => Self::SubPlan,
            "InitPlan" => Self::InitPlan,
            "CTE" => Self::Cte,
            other => Self::Other(other.into()),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanExecutionMode {
    Row,
    Batch,
    Other(String),
}
impl PlanExecutionMode {
    pub fn from_source(value: &str) -> Self {
        if value.eq_ignore_ascii_case("row") {
            Self::Row
        } else if value.eq_ignore_ascii_case("batch") {
            Self::Batch
        } else {
            Self::Other(value.into())
        }
    }
}

/// Native numbers and older property strings use the same validity rules. Invalid evidence is unknown, never zero.
pub fn nonnegative(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}
pub fn fraction(value: f64) -> Option<f64> {
    nonnegative(value).filter(|v| *v <= 1.0)
}
pub fn percentage(value: f64) -> Option<f64> {
    nonnegative(value).filter(|v| *v <= 100.0)
}
pub fn whole(value: f64) -> Option<f64> {
    nonnegative(value).filter(|v| v.fract() == 0.0)
}
pub fn thread_count(value: f64) -> Option<u32> {
    count(value).filter(|n| *n > 0)
}
pub fn count(value: f64) -> Option<u32> {
    (value.is_finite() && value >= 0.0 && value <= u32::MAX as f64 && value.fract() == 0.0).then_some(value as u32)
}

// The wire remains an optional JSON number. Serde validates imported facts while TypeScript uses that same shape.
mod nonnegative_number {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.and_then(nonnegative))
    }
}
mod fraction_number {
    pub use super::nonnegative_number::serialize;
    use super::*;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.and_then(fraction))
    }
}
mod percentage_number {
    pub use super::nonnegative_number::serialize;
    use super::*;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.and_then(percentage))
    }
}

/// The pages a node spilled, by its runtime counters.
pub(crate) fn spill_pages(node: &crate::PlanNode) -> Option<f64> {
    node.runtime.as_ref().and_then(|r| r.spill_pages).and_then(nonnegative)
}

/// Number of participating workers/processes, their total rows and the busiest worker's rows, from the actual worker
/// vector in NodeRuntime.
pub(crate) fn parallel_distribution(node: &crate::PlanNode, engine: crate::Engine) -> Option<(usize, f64, f64)> {
    if !matches!(engine, crate::Engine::Mssql | crate::Engine::Postgres) {
        return None;
    }
    let mut rows = node.runtime.as_ref().map(|r| r.thread_rows.clone()).filter(|rows| rows.len() >= 2)?;
    if rows.iter().any(|v| nonnegative(*v).is_none()) {
        return None;
    }
    if engine == crate::Engine::Postgres {
        let worker_total: f64 = rows.iter().sum();
        let leader = (node.act_rows.unwrap_or(worker_total) - worker_total).max(0.0);
        rows.push(leader);
    }
    let total: f64 = rows.iter().sum();
    let busiest = rows.iter().copied().fold(0.0, f64::max);
    nonnegative(total)?;
    (rows.len() >= 2).then_some((rows.len(), total, busiest))
}

mod count_number {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.and_then(count))
    }
}

mod trigger_measurements {
    use super::*;
    #[derive(Deserialize)]
    struct Input {
        name: String,
        relation: Option<String>,
        time_ms: Option<f64>,
        calls: Option<f64>,
    }
    pub fn serialize<S: serde::Serializer>(value: &[crate::actual::TriggerTiming], s: S) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<crate::actual::TriggerTiming>, D::Error> {
        Ok(Vec::<Input>::deserialize(d)?
            .into_iter()
            .filter_map(|t| {
                Some(crate::actual::TriggerTiming {
                    name: t.name,
                    relation: t.relation,
                    time_ms: t.time_ms.and_then(nonnegative)?,
                    calls: t.calls.and_then(whole),
                })
            })
            .collect())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanIndexColumns {
    pub name: String,
    pub columns: Vec<String>,
    // Native DocDB hash group; an empty group is range sharded. None means no native sharding evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hash_columns: Option<Vec<String>>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanAutomaticIndex {
    pub table: String,
    pub columns: Vec<String>,
}
impl PlanAutomaticIndex {
    /// SQLite's `table(col, col)` description.
    pub fn from_description(value: &str) -> Option<Self> {
        let (table, cols) = value.split_once('(')?;
        let columns = cols
            .trim_end_matches(')')
            .split(',')
            .filter_map(|c| {
                let name: String = c.trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                (!name.is_empty()).then_some(name)
            })
            .collect();
        Some(Self { table: table.trim().into(), columns })
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanConversionIssue {
    SeekPlan,
    CardinalityEstimate,
    Other(String),
}
impl PlanConversionIssue {
    pub fn from_source(value: &str) -> Self {
        match value {
            "Seek Plan" => Self::SeekPlan,
            "Cardinality Estimate" => Self::CardinalityEstimate,
            v => Self::Other(v.into()),
        }
    }
    fn label(&self) -> &str {
        match self {
            Self::SeekPlan => "Seek Plan",
            Self::CardinalityEstimate => "Cardinality Estimate",
            Self::Other(v) => v,
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanConversion {
    pub issue: PlanConversionIssue,
    pub expression: String,
}
impl PlanConversion {
    pub fn text(&self) -> String {
        format!("{}: {}", self.issue.label(), self.expression)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanChangedColumn {
    pub schema: Option<String>,
    pub table: String,
    pub column: String,
    pub data_type: String,
}
fn is_false(value: &bool) -> bool {
    !*value
}

/// The native recorded run used to estimate a statement's plan.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanSampleSource {
    Digest,
    History,
    SlowLog,
}

impl PlanSampleSource {
    pub fn sentence(self) -> String {
        match self {
            Self::Digest => crate::t!("provider.mysql.plans.sample.digest"),
            Self::History => crate::t!("provider.mysql.plans.sample.history"),
            Self::SlowLog => crate::t!("provider.mysql.plans.sample.slow_log"),
        }
    }
}
