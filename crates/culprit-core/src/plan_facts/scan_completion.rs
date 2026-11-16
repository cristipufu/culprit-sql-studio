//! Source-established full table-scan counters preserve their original worker population and exact integers.
//! SQL Server's ActualEndOfScans counts exhausted operator streams, not automatically complete input parameters:
//! https://learn.microsoft.com/sql/relational-databases/showplan-logical-and-physical-operators-reference
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

/// Retained analysis policy, not a claim about the database's maximum degree of parallelism.
pub const MAX_PLAN_SCAN_THREADS: usize = 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanScanThread {
    pub thread: u32,
    pub executions: Option<String>,
    pub end_of_scans: Option<String>,
    pub rebinds: Option<String>,
    pub rewinds: Option<String>,
}

impl PlanScanThread {
    fn is_valid(&self) -> bool {
        [&self.executions, &self.end_of_scans, &self.rebinds, &self.rewinds]
            .into_iter()
            .flatten()
            .all(|value| value.len() <= 20 && value.parse::<u64>().is_ok_and(|count| count.to_string() == *value))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanScanCompletion {
    /// Native operator parallelism, independent of which worker counters happened to be present.
    pub parallel: Option<bool>,
    pub threads: Vec<PlanScanThread>,
}

impl PlanScanCompletion {
    pub fn is_valid(&self) -> bool {
        let mut threads = BTreeSet::new();
        !self.threads.is_empty()
            && self.threads.len() <= MAX_PLAN_SCAN_THREADS
            && self.threads.iter().all(|thread| thread.is_valid() && threads.insert(thread.thread))
    }

    /// Only the original serial coordinator's one exhausted execution can participate in an input-size proof.
    /// Callers must independently qualify the input, full access, actual rows and its surrounding plan path.
    pub fn is_single_completed_serial_scan(&self) -> bool {
        self.is_valid()
            && self.parallel == Some(false)
            && matches!(self.threads.as_slice(), [thread]
                if thread.thread == 0
                    && thread.executions.as_deref() == Some("1")
                    && thread.end_of_scans.as_deref() == Some("1")
                    && thread.rebinds.as_deref().is_none_or(|value| matches!(value, "0" | "1"))
                    && thread.rewinds.as_deref().is_none_or(|value| value == "0"))
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<PlanScanCompletion>, D::Error> {
    Ok(Option::<PlanScanCompletion>::deserialize(deserializer)?.filter(PlanScanCompletion::is_valid))
}

#[cfg(test)]
mod tests;
