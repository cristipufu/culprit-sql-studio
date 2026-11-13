//! Native temporary resource work has several meanings. Counts and space use do not establish spill duration,
//! memory exhaustion, peak concurrent demand or the operator that performed the work.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct TemporaryWorkExplanation {
    pub title: String,
    pub summary: String,
    pub metrics: Vec<crate::Metric>,
    pub limitations: Vec<String>,
    pub investigation: String,
    pub verify: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum StatementTemporaryWork {
    /// Statement resource totals can include planning and execution. Reported zero timings do not establish
    /// complete historical timing coverage, and parallel I/O time is not an exclusive elapsed-time component.
    BlockIo {
        source: String,
        bytes_per_block: Option<u32>,
        read_blocks: Option<String>,
        written_blocks: Option<String>,
        read_time_ms: Option<f64>,
        write_time_ms: Option<f64>,
    },
    /// Native internal-table/pass counts are neither byte amounts nor durations. A disk table can be created
    /// directly; zero disk tables does not establish that all temporary work stayed in memory.
    TableCreation {
        source: String,
        created_tables: Option<String>,
        disk_tables: Option<String>,
        sort_merge_passes: Option<String>,
    },
    /// Weighted sum of native per-execution space-use observations, such as Query Store averages. This is not
    /// write traffic, simultaneous allocation, retained footprint or a measured peak for the selected interval.
    SpaceUsage { source: String, bytes_per_page: u32, usage_pages_total: f64 },
}

impl StatementTemporaryWork {
    /// Native zeros are measurements; source names and unit metadata alone are not.
    pub fn has_measurements(&self) -> bool {
        if !self.is_valid() {
            return false;
        }
        match self {
            Self::BlockIo { read_blocks, written_blocks, read_time_ms, write_time_ms, .. } => {
                read_blocks.is_some() || written_blocks.is_some() || read_time_ms.is_some() || write_time_ms.is_some()
            }
            Self::TableCreation { created_tables, disk_tables, sort_merge_passes, .. } => {
                created_tables.is_some() || disk_tables.is_some() || sort_merge_passes.is_some()
            }
            Self::SpaceUsage { .. } => true,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::BlockIo { source, bytes_per_block, read_blocks, written_blocks, read_time_ms, write_time_ms } => {
                valid_source(source)
                    && bytes_per_block.is_none_or(valid_page_size)
                    && valid_counts([read_blocks, written_blocks])
                    && [read_time_ms, write_time_ms].into_iter().all(|value| value.is_none_or(nonnegative))
            }
            Self::TableCreation { source, created_tables, disk_tables, sort_merge_passes } => {
                valid_source(source) && valid_counts([created_tables, disk_tables, sort_merge_passes])
            }
            Self::SpaceUsage { source, bytes_per_page, usage_pages_total } => {
                valid_source(source) && valid_page_size(*bytes_per_page) && nonnegative(*usage_pages_total)
            }
        }
    }

    /// A positive source value establishes observed work. Absent values and native zeros establish no converse.
    pub fn has_observed_work(&self) -> bool {
        if !self.has_measurements() {
            return false;
        }
        match self {
            Self::BlockIo { read_blocks, written_blocks, read_time_ms, write_time_ms, .. } => {
                positive_counts([read_blocks, written_blocks])
                    || [read_time_ms, write_time_ms].into_iter().any(|value| value.is_some_and(|value| value > 0.0))
            }
            Self::TableCreation { created_tables, disk_tables, sort_merge_passes, .. } => {
                positive_counts([created_tables, disk_tables, sort_merge_passes])
            }
            Self::SpaceUsage { usage_pages_total, .. } => *usage_pages_total > 0.0,
        }
    }
}

fn valid_source(source: &str) -> bool {
    !source.trim().is_empty() && source.len() <= 512
}

fn valid_page_size(bytes: u32) -> bool {
    bytes.is_power_of_two() && (512..=1_048_576).contains(&bytes)
}

fn valid_counts<'a>(values: impl IntoIterator<Item = &'a Option<String>>) -> bool {
    values.into_iter().flatten().all(|value| value.len() <= 20 && value.parse::<u64>().is_ok())
}

fn positive_counts<'a>(values: impl IntoIterator<Item = &'a Option<String>>) -> bool {
    values.into_iter().flatten().any(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
}

fn nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

#[cfg(test)]
mod tests;
