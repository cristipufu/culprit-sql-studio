//! Page-server counters belong to one native operator's runtime population, not a current incident or cache window.
//! The Showplan schema places these optional unsigned integers on RunTimeCountersPerThread:
//! https://schemas.microsoft.com/sqlserver/2004/07/showplan/sql2022/showplanxml.xsd
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

use crate::plan_facts::MAX_PLAN_SCAN_THREADS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanPageServerMetric {
    Reads,
    ReadAheads,
    LobReads,
    LobReadAheads,
}
impl PlanPageServerMetric {
    pub const ALL: [Self; 4] = [Self::Reads, Self::ReadAheads, Self::LobReads, Self::LobReadAheads];
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum PlanPageServerIoIssue {
    MissingCounter { metric: PlanPageServerMetric },
    InvalidCounter { metric: PlanPageServerMetric },
    CounterOverflow { metric: PlanPageServerMetric },
    InvalidThread,
    DuplicateThread,
    ThreadLimit,
    MultiplePopulations,
}
impl PlanPageServerIoIssue {
    fn affects(self, metric: PlanPageServerMetric) -> bool {
        match self {
            Self::MissingCounter { metric: affected }
            | Self::InvalidCounter { metric: affected }
            | Self::CounterOverflow { metric: affected } => affected == metric,
            _ => true,
        }
    }
}

/// Exact page totals across this operator's unique native threads. Demand, read-ahead and LOB remain independent.
/// These counters have no independently established UTC interval, reset marker or execution-group identity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PlanPageServerIo {
    /// Runtime rows retained up to the existing thread policy. ThreadLimit means the population is incomplete.
    pub threads_observed: u32,
    pub reads: Option<String>,
    pub read_aheads: Option<String>,
    pub lob_reads: Option<String>,
    pub lob_read_aheads: Option<String>,
    /// Bounded, deduplicated reasons for unavailable totals; never the malformed native text.
    pub issues: Vec<PlanPageServerIoIssue>,
}
impl PlanPageServerIo {
    pub fn value(&self, metric: PlanPageServerMetric) -> Option<&str> {
        match metric {
            PlanPageServerMetric::Reads => self.reads.as_deref(),
            PlanPageServerMetric::ReadAheads => self.read_aheads.as_deref(),
            PlanPageServerMetric::LobReads => self.lob_reads.as_deref(),
            PlanPageServerMetric::LobReadAheads => self.lob_read_aheads.as_deref(),
        }
    }

    pub fn is_valid(&self) -> bool {
        // Four fields can each have three distinct issues, plus four population issues.
        const MAX_ISSUES: usize = PlanPageServerMetric::ALL.len() * 3 + 4;
        let unique: BTreeSet<_> = self.issues.iter().collect();
        (1..=MAX_PLAN_SCAN_THREADS as u32).contains(&self.threads_observed)
            && self.issues.len() <= MAX_ISSUES
            && unique.len() == self.issues.len()
            && (!self.issues.contains(&PlanPageServerIoIssue::ThreadLimit)
                || self.threads_observed == MAX_PLAN_SCAN_THREADS as u32)
            && PlanPageServerMetric::ALL.into_iter().all(|metric| {
                let affected = self.issues.iter().any(|issue| issue.affects(metric));
                match self.value(metric) {
                    Some(value) => !affected && exact_count(value).is_some(),
                    None => affected,
                }
            })
    }
}

/// Borrow the actual native attributes; callers neither invent missing counters nor pre-convert to floating point.
pub struct PlanPageServerThread<'a> {
    pub thread: Option<&'a str>,
    pub reads: Option<&'a str>,
    pub read_aheads: Option<&'a str>,
    pub lob_reads: Option<&'a str>,
    pub lob_read_aheads: Option<&'a str>,
}

#[derive(Default)]
pub struct PlanPageServerIoBuilder {
    threads: BTreeSet<u32>,
    observed: u32,
    mentioned: bool,
    totals: [u64; 4],
    issues: BTreeSet<PlanPageServerIoIssue>,
}
impl PlanPageServerIoBuilder {
    pub fn observe(&mut self, point: PlanPageServerThread<'_>) {
        let values = [point.reads, point.read_aheads, point.lob_reads, point.lob_read_aheads];
        self.mentioned |= values.iter().any(Option::is_some);
        if self.observed as usize == MAX_PLAN_SCAN_THREADS {
            self.issues.insert(PlanPageServerIoIssue::ThreadLimit);
            return;
        }
        self.observed += 1;
        match point.thread.and_then(|value| value.parse::<i32>().ok()).and_then(|value| u32::try_from(value).ok()) {
            None => {
                self.issues.insert(PlanPageServerIoIssue::InvalidThread);
            }
            Some(thread) if !self.threads.insert(thread) => {
                self.issues.insert(PlanPageServerIoIssue::DuplicateThread);
            }
            Some(_) => {}
        }
        for ((metric, total), value) in PlanPageServerMetric::ALL.into_iter().zip(&mut self.totals).zip(values) {
            let Some(raw) = value else {
                self.issues.insert(PlanPageServerIoIssue::MissingCounter { metric });
                continue;
            };
            let Some(value) = exact_count(raw) else {
                self.issues.insert(PlanPageServerIoIssue::InvalidCounter { metric });
                continue;
            };
            match total.checked_add(value) {
                Some(value) => *total = value,
                None => {
                    self.issues.insert(PlanPageServerIoIssue::CounterOverflow { metric });
                }
            }
        }
    }

    pub fn mark_multiple_populations(&mut self) {
        self.issues.insert(PlanPageServerIoIssue::MultiplePopulations);
    }

    pub fn finish(self) -> Option<PlanPageServerIo> {
        if !self.mentioned {
            return None;
        }
        let value = |metric: PlanPageServerMetric, index: usize| {
            (!self.issues.iter().any(|issue| issue.affects(metric))).then(|| self.totals[index].to_string())
        };
        let result = PlanPageServerIo {
            threads_observed: self.observed,
            reads: value(PlanPageServerMetric::Reads, 0),
            read_aheads: value(PlanPageServerMetric::ReadAheads, 1),
            lob_reads: value(PlanPageServerMetric::LobReads, 2),
            lob_read_aheads: value(PlanPageServerMetric::LobReadAheads, 3),
            issues: self.issues.into_iter().collect(),
        };
        debug_assert!(result.is_valid());
        Some(result)
    }
}

fn exact_count(value: &str) -> Option<u64> {
    (value.len() <= 20).then(|| value.parse::<u64>().ok()).flatten().filter(|count| count.to_string() == value)
}

/// Saved direct models use the same validation as native construction and pure presentation.
pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<PlanPageServerIo>>, D::Error> {
    let value = Option::<Box<PlanPageServerIo>>::deserialize(deserializer)?;
    if value.as_ref().is_some_and(|value| !value.is_valid()) {
        return Err(serde::de::Error::custom(crate::telemetry::CollectionStop::InvalidPage.error()));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
