//! Detector facts used by cross-finding decisions. Human metric labels and prose are presentation, never identifiers.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct FindingFacts {
    /// Advice intent selected with the evidence, independent of translated prose or SQL comments.
    #[serde(default, skip_serializing_if = "AdviceFacts::is_empty")]
    pub advice: AdviceFacts,
    /// The evidence's semantic role. Public finding IDs remain opaque identifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cause: Option<FindingCause>,
    /// Whether the linked statements establish the finding or merely provide context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workload_dependency: Option<WorkloadDependency>,
    /// The affected table when the displayed subject is a particular index instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub table: Option<String>,
    /// The cleanup holder's native identity, independent of the database display subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup_holder: Option<String>,
    /// The native cleanup horizon selected by the diagnosis; data and catalog holds are independent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup_horizon: Option<crate::telemetry::CleanupHorizon>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup_retention_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup_visibility_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup_tied_holders: Option<u32>,
    pub histogram_columns: Vec<String>,
    pub index_keys: Vec<String>,
    pub lock_wait: Option<FindingLockWait>,
    pub plan_change: Option<FindingPlanChange>,
    // A retained fast-plan baseline for verifying recovery after it leaves the selected window.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan_baseline: Option<PlanBaseline>,
    pub recommended_plan: Option<FindingPlanRecommendation>,
    /// Recorded performance variation selects an investigation; it does not establish parameter sensitivity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_variation: Option<FindingRuntimeVariation>,
    pub bloat: Option<FindingBloat>,
    pub log_write: Option<FindingLogWrite>,
    pub large_rows: Option<FindingLargeRows>,
    /// The files a storage finding is about, including each constituent of a merged log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_files: Vec<crate::signals::FileReference>,
    /// Its first verify step says there is nothing to check for now, and why: refine keeps a check's own such step,
    /// and the brief adds no step of its own to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub nothing_to_check: Option<NothingToCheck>,
    // The exact migration statement and its source location. Prose never identifies executable edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statement: Option<StatementEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uncosted_plan_explained: Option<bool>,
}

/// Observed runtime shape, independent of translated explanations, suspected causes or proposed plan changes.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum FindingRuntimeVariation {
    PlanAverages { faster_plan: String, slower_plan: String },
    RuntimeTail,
}

/// Evidence used to relate findings, independent of a check's public naming convention.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FindingCause {
    PredicateRewrite,
    Sort,
    Spill,
    IndexRequest,
    AutomaticIndex,
    ForcedPlanScan,
    TableStatistics,
    DeadRows,
    LargeRows,
    HotUpdates,
    PlanCache,
    StatisticsCapture,
    TableRewrite,
    VolatileDefault,
    RequiredColumn,
    ConstraintValidation,
    ForeignKeyIndex,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WorkloadDependency {
    CurrentStatements,
    Independent,
}

/// An exact SQL source range: zero-based UTF-16 start and exclusive end, plus its one-based line number.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementEvidence {
    pub sql: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
}

/// Why a finding's verify step says there is nothing to check for now ([`FindingFacts::nothing_to_check`]).
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum NothingToCheck {
    /// The tables it is about are gone.
    Gone,
    /// Its statements stopped, or the optimizer stopped asking: nothing until they come back.
    UntilItReturns,
    /// The table is too small for the change to matter yet.
    WhileSmall,
}
impl FindingFacts {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingLockWait {
    /// Accumulated native lock waiting, not an exclusive part of statement elapsed time.
    pub ms: f64,
    /// SQL shape and the current setting make row versioning worth investigating; lock resources are unproven.
    pub versioning_candidate: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingPlanChange {
    pub from: String,
    pub to: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingPlanRecommendation {
    pub plan: String,
    pub cpu_saved_share: Option<f64>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingBloat {
    pub bytes: f64,
    pub share: f64,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingLogWrite {
    pub bytes: f64,
    pub per_row: Option<f64>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FindingLargeRows {
    pub rows: f64,
    pub size: f64,
    pub large: f64,
}

/// No SQL or parameters: safe to keep in a digest between scans.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PlanBaseline {
    pub query: String,
    pub mean_ms: f64,
    // Last observed run of the regressed plan. Recovery needs fresh runs after this observation.
    pub observed_until: String,
}

pub const PLAN_VARIANT_MIN_RUNS: f64 = 5.0;

impl PlanBaseline {
    pub(crate) fn recovery_problem(&self, snapshot: &crate::Snapshot) -> Option<String> {
        let unknown = || {
            Some("Fresh, sufficiently sampled plan runs have not established recovery against the earlier fast-plan baseline.".into())
        };
        let Some(w) = snapshot.workload.as_ref().filter(|w| w.kind() == Some(crate::WorkloadKind::QueryStore)) else {
            return unknown();
        };
        if let Some(reason) = snapshot.source_problem(&[crate::source_id::QUERY_STORE]) {
            return Some(reason);
        }
        if snapshot
            .source_outcome(crate::source_id::QUERY_STORE)
            .is_some_and(|o| o.state == crate::SourceState::NotApplicable)
        {
            return unknown();
        }
        let Some(q) = w.queries.iter().find(|q| q.id == self.query) else { return unknown() };
        let Some((start, end)) = w
            .window_start
            .as_deref()
            .and_then(crate::format::epoch_seconds)
            .zip(w.window_end.as_deref().and_then(crate::format::epoch_seconds))
        else {
            return unknown();
        };
        let Some(observed) = crate::format::epoch_seconds(&self.observed_until) else { return unknown() };
        if !self.mean_ms.is_finite() || self.mean_ms < 0.0 || q.workload_plans().is_empty() {
            return unknown();
        }
        let recovered = q.workload_plans().iter().all(|p| {
            let span = p
                .first_seen
                .as_deref()
                .and_then(crate::format::epoch_seconds)
                .zip(p.last_seen.as_deref().and_then(crate::format::epoch_seconds));
            p.executions.is_finite()
                && p.executions >= PLAN_VARIANT_MIN_RUNS
                && p.mean_time_ms.is_finite()
                && p.mean_time_ms >= 0.0
                && span.is_some_and(|(first, last)| first > observed && last >= first && first >= start && last <= end)
                && (p.mean_time_ms < self.mean_ms * 2.0 || p.mean_time_ms - self.mean_ms < 5.0)
        });
        if recovered { None } else { unknown() }
    }
}

/// Where the application change belongs and what evidence must precede it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct AdviceFacts {
    pub disposition: AdviceDisposition,
    pub application: ApplicationAdvice,
    pub diagnostic_cleanup: bool,
    pub upkeep_on_connection_close: bool,
    pub statement_quote: Option<StatementQuote>,
    pub running_totals_explained: bool,
    pub disappears_after_rescan: bool,
}
impl AdviceFacts {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AdviceDisposition {
    #[default]
    Change,
    NoChange,
    GatherRuntimeEvidence,
    OptionalUpkeep,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ApplicationAdvice {
    #[default]
    Unspecified,
    Alternative,
    Preferred,
    SessionSettings,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StatementQuote {
    Summary,
    Caveat,
}
