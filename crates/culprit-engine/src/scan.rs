//! One scan's collection, history enrichment and persistence. This service needs a report directory and checks,
//! never connection configuration or secrets. Workspace resolves the provider and handles rejected credentials.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use culprit_core::{
    Check, CollectorNote, ErrorCode, PopulationSnapshots, Provider, Report, Result, RuntimePopulationCatalog, Snapshot,
    SnapshotOptions,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{analysis, history, query_log, reports, trends};

pub(crate) struct QueryLog<'a> {
    pub path: &'a Path,
    pub discovered: bool,
}

pub(crate) struct Scan<'a> {
    dir: PathBuf,
    checks: &'a [Arc<dyn Check>],
    started: Instant,
    taken_at: chrono::DateTime<chrono::Utc>,
}

/// Independent reports from one bounded acquisition. Catalog membership is not a combined runtime denominator.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PopulationReports {
    /// Completed reports are allocated by the analysis worker and stay owned on the heap across async boundaries.
    pub requested: Box<Report>,
    #[allow(clippy::vec_box, reason = "moving completed reports between nested async frames must stay pointer-sized")]
    pub populations: Vec<Box<Report>>,
    pub catalog: Option<RuntimePopulationCatalog>,
}

struct SharedEvidence {
    deadlocks: Option<culprit_core::DeadlockReport>,
    notes: Vec<CollectorNote>,
    recorded: Vec<culprit_core::ChangeEvent>,
    collection: culprit_core::CollectionProvenance,
}

impl<'a> Scan<'a> {
    /// Start the clock before authentication, as the public scan operation always has.
    pub fn begin(dir: PathBuf, checks: &'a [Arc<dyn Check>]) -> Self {
        Self { dir, checks, started: Instant::now(), taken_at: chrono::Utc::now() }
    }

    /// Finish native enrichment and return only facts. Analysis and persistence receive no provider.
    pub async fn collect_inputs(
        self,
        provider: &dyn Provider,
        snapshots: PopulationSnapshots,
        name: &str,
        options: &SnapshotOptions,
        log: Option<QueryLog<'_>>,
    ) -> Result<CollectedPopulationScan> {
        snapshots.validate(options)?;
        let PopulationSnapshots { mut requested, populations, catalog } = snapshots;
        let snapshot = &mut requested;
        // What was asked for, recorded once for every provider: the saved report is found by its window.
        if snapshot.options.is_none() {
            snapshot.options = Some(options.clone());
        }
        record_unreported_optional_sources(snapshot, options);
        // An application log has no native execution-group identity. It can supply the default view, while an
        // explicit native selection must retain its own unavailable evidence instead of borrowing log timings.
        if let Some(log) = log.filter(|_| options.workload_population.is_none() || snapshot.workload.is_some()) {
            query_log::attach(provider, snapshot, log.path, options, log.discovered).await?;
        }
        self.stamp(snapshot, name)?;
        let dir = self.dir.clone();
        // What the database dated itself since the previous scan (schema, settings), for the change timeline.
        let summary_dir = dir.clone();
        let selectors = std::iter::once(&requested)
            .chain(populations.iter())
            .map(|snapshot| snapshot.options.as_ref().and_then(|options| options.workload_population.clone()))
            .collect::<Vec<_>>();
        let previous = crate::run_blocking(move || {
            selectors
                .iter()
                .map(|population| reports::latest_selected_summary(&summary_dir, None, population.as_ref()))
                .collect::<Result<Vec<_>>>()
        })
        .await?;
        let since = previous
            .iter()
            .map(|previous| previous.as_ref().map(|summary| summary.taken_at.clone()))
            .collect::<Option<Vec<_>>>()
            .and_then(|times| times.into_iter().min())
            .unwrap_or_else(|| (self.taken_at - chrono::Duration::days(trends::HISTORY_DAYS as i64)).to_rfc3339());
        // Deadlocks over the scan's window, a day at least: graphs where the engine keeps them, Postgres's counter.
        // Neither fails the scan.
        let deadlock_hours = (f64::from(options.window_in_minutes()) / 60.0).clamp(24.0, 24.0 * 30.0);
        let (deadlocks, recorded) = tokio::join!(
            crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, provider.deadlocks(deadlock_hours)),
            crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, provider.recorded_changes(&since))
        );
        let mut auxiliary_notes = Vec::new();
        let deadlocks = match deadlocks {
            Ok(found) => Some(found),
            Err(e) if e.code == ErrorCode::Unsupported => None,
            Err(e) if crate::source_operation::native_shutdown_uncertain(&e) => {
                return Err(e);
            }
            Err(e) => {
                auxiliary_notes.push(CollectorNote {
                    message_source: None,
                    collector: "deadlocks".into(),
                    message: e.message,
                    table: None,
                });
                None
            }
        };
        crate::ensure_source_quiet(provider)?;
        let recorded = match recorded {
            Ok(found) => found,
            Err(e) if e.code == ErrorCode::Unsupported => Vec::new(),
            Err(e) if crate::source_operation::native_shutdown_uncertain(&e) => {
                return Err(e);
            }
            Err(e) => {
                auxiliary_notes.push(CollectorNote {
                    message_source: None,
                    collector: "changes".into(),
                    message: e.message,
                    table: None,
                });
                Vec::new()
            }
        };
        let evidence = SharedEvidence {
            deadlocks,
            notes: auxiliary_notes,
            recorded,
            collection: culprit_core::CollectionProvenance {
                collector: provider.collector_version(),
                started_at: requested.taken_at.clone(),
                completed_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            },
        };
        let mut previous = previous.into_iter();
        let requested = self.prepare(requested, name, options, &evidence, previous.next().flatten())?;
        let populations = populations
            .into_iter()
            .zip(previous)
            .map(|(snapshot, previous)| {
                let selected = snapshot
                    .options
                    .clone()
                    .ok_or_else(|| culprit_core::telemetry::CollectionStop::InvalidPage.error())?;
                self.prepare(snapshot, name, &selected, &evidence, previous)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(CollectedPopulationScan { requested, populations, catalog })
    }

    fn stamp(&self, snapshot: &mut Snapshot, name: &str) -> Result<()> {
        let id = self.taken_at.format("%Y%m%dT%H%M%S%.3fZ").to_string();
        snapshot.id = match snapshot.options.as_ref().and_then(|options| options.workload_population.as_ref()) {
            Some(population) => format!("{id}-population-{}", reports::population_key(population)?),
            None => id,
        };
        snapshot.taken_at = self.taken_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        snapshot.connection = name.to_owned();
        snapshot.duration_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }

    fn prepare(
        &self,
        mut snapshot: Snapshot,
        name: &str,
        options: &SnapshotOptions,
        evidence: &SharedEvidence,
        previous: Option<reports::ReportSummary>,
    ) -> Result<CollectedScan> {
        self.stamp(&mut snapshot, name)?;
        record_unreported_optional_sources(&mut snapshot, options);
        if let Some(deadlocks) = &evidence.deadlocks {
            snapshot.deadlocks = Some(deadlocks.clone());
        }
        snapshot.notes.extend_from_slice(&evidence.notes);
        Ok(CollectedScan {
            dir: self.dir.clone(),
            checks: self.checks.to_vec(),
            snapshot,
            recorded: evidence.recorded.clone(),
            previous,
            collection: evidence.collection.clone(),
        })
    }
}

/// The native owner has completed before any of these independent inputs can be analyzed or persisted.
pub(crate) struct CollectedPopulationScan {
    requested: CollectedScan,
    populations: Vec<CollectedScan>,
    catalog: Option<RuntimePopulationCatalog>,
}

impl CollectedPopulationScan {
    pub async fn finish(self) -> Result<PopulationReports> {
        let requested = self.requested.finish().await?;
        let mut populations = Vec::with_capacity(self.populations.len());
        for collected in self.populations {
            populations.push(collected.finish().await?);
        }
        Ok(PopulationReports { requested, populations, catalog: self.catalog })
    }
}

/// A provider may omit an optional reader. An explicit request must still explain the missing qualification,
/// without guessing support or replacing native evidence and coverage supplied by an embedding provider.
fn record_unreported_optional_sources(snapshot: &mut Snapshot, options: &SnapshotOptions) {
    use culprit_core::{SourceOutcome, SourceState, source_id};
    let requested_sources = [
        (
            options.collect_request_memory,
            snapshot.pressure.as_ref().is_some_and(|pressure| pressure.request_memory.is_some()),
            source_id::CURRENT_REQUEST_MEMORY,
        ),
        (options.collect_session_memory, snapshot.operations.session_memory.is_some(), source_id::SESSION_MEMORY),
        (options.collect_page_wait_targets, snapshot.operations.page_waits.is_some(), source_id::PAGE_WAIT_TARGETS),
        (options.collect_task_space, snapshot.operations.task_space.is_some(), source_id::TASK_SPACE),
        (options.collect_memory_failures, snapshot.operations.memory_failures.is_some(), source_id::MEMORY_FAILURES),
        (
            options.collect_native_failures,
            snapshot.operations.native_failure_diagnostics_assessed(&snapshot.sources),
            source_id::NATIVE_FAILURES,
        ),
    ];
    for (requested, has_evidence, source) in requested_sources {
        if !requested || has_evidence || snapshot.sources.iter().any(|outcome| outcome.source == source) {
            continue;
        }
        let reason = culprit_core::message!("provider.optional_source.unreported");
        snapshot.notes.push(CollectorNote::recorded(source, reason.clone()));
        snapshot.sources.push(SourceOutcome {
            source: source.into(),
            state: SourceState::Unavailable,
            reason: Some(reason.render()),
            reason_message: Some(reason),
        });
    }
}

/// All native work has ended. Analysis and persistence cannot reopen a provider.
pub(crate) struct CollectedScan {
    dir: PathBuf,
    checks: Vec<Arc<dyn Check>>,
    snapshot: Snapshot,
    recorded: Vec<culprit_core::ChangeEvent>,
    previous: Option<reports::ReportSummary>,
    collection: culprit_core::CollectionProvenance,
}
impl CollectedScan {
    pub async fn finish(self) -> Result<Box<Report>> {
        let Self { dir, checks, mut snapshot, recorded, previous, collection } = self;
        crate::run_blocking(move || {
            let collected = snapshot.clone();
            let recorded_evidence = recorded.clone();
            let previous = previous
                .filter(|p| p.id != snapshot.id)
                .and_then(|p| reports::load_snapshot(&dir.join(format!("{}.json", p.id))).ok());
            // A replica's usage counters are its own reads: set them aside before anything compares or judges them.
            history::set_aside_replica_usage(&mut snapshot);
            let same_database = previous.as_ref().filter(|p| history::same_database(p, &snapshot));
            let evictions = trends::evictions(&snapshot);
            if snapshot.workload.as_ref().is_some_and(|w| w.is_cumulative()) {
                // A source that can't say when it started counting (pg_stat_statements before 14) gets its window from the
                // scan before.
                trends::infer_window(&mut snapshot, same_database);
                let history = reports::counters_history(&dir, &snapshot.taken_at);
                if snapshot.workload.as_ref().is_some_and(|w| w.timeline.is_none()) {
                    trends::from_scans(&mut snapshot, evictions, &history);
                }
                // Statements whose source doesn't say when they last ran get it from the scans that saw them grow.
                trends::last_seen_from_scans(&mut snapshot, &history);
            }
            // This scan's changes come before the checks: an index the scans saw appear this week isn't unused yet, nor
            // one whose usage counters started over.
            let changes = history::diff(previous.as_ref(), &snapshot, recorded);
            let age_inputs = history::date_new_indexes_traced(&dir, &mut snapshot, &changes);
            // Lock escalations, index contention and operational counters are counted since startup with no time for each:
            // compare against the earliest compatible scan covering the selected window.
            reports::apply_counter_baselines(&dir, &mut snapshot, previous.as_ref());
            reports::apply_compilation_baseline(&dir, &mut snapshot, previous.as_ref());
            // pg_stat_statements' evictions are counted since its last reset: the oldest of Culprit's scans a day or more
            // back tells the rate they go at now, else the previous scan's count tells new ones.
            snapshot.evictions_before = reports::evictions_baseline(&dir, &snapshot).or_else(|| {
                previous
                    .as_ref()
                    .filter(|p| history::same_database(p, &snapshot))
                    .and_then(|p| culprit_core::CountBaseline::from_setting(p, &snapshot, trends::EVICTIONS_SETTING))
            });
            // How big each table was a day or more ago tells how fast it grows.
            snapshot.sizes_before = history::growth_baseline(&dir, &snapshot);
            let report =
                Box::new(analysis::collected(snapshot, collected, collection, recorded_evidence, age_inputs, &checks));
            reports::save(&dir, &report)?;
            history::keep_scan(&dir, &report.snapshot, previous.as_ref(), &changes);
            Ok(report)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded() -> Snapshot {
        serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap()
    }

    #[test]
    fn an_ignored_diagnostic_opt_in_reports_unknown_coverage_without_inventing_an_empty_read() {
        let mut snapshot = recorded();
        let original = snapshot.sources.clone();
        record_unreported_optional_sources(&mut snapshot, &SnapshotOptions::default());
        assert_eq!(snapshot.sources, original, "default scans acquire no new diagnostic source");
        let options = SnapshotOptions {
            collect_request_memory: true,
            collect_session_memory: true,
            collect_page_wait_targets: true,
            collect_task_space: true,
            collect_memory_failures: true,
            collect_native_failures: true,
            ..Default::default()
        };
        record_unreported_optional_sources(&mut snapshot, &options);
        let expected = snapshot.sources.clone();
        for source in [
            culprit_core::source_id::CURRENT_REQUEST_MEMORY,
            culprit_core::source_id::SESSION_MEMORY,
            culprit_core::source_id::PAGE_WAIT_TARGETS,
            culprit_core::source_id::TASK_SPACE,
            culprit_core::source_id::MEMORY_FAILURES,
            culprit_core::source_id::NATIVE_FAILURES,
        ] {
            let outcome = snapshot.sources.iter().find(|outcome| outcome.source == source).unwrap();
            assert_eq!(outcome.state, culprit_core::SourceState::Unavailable);
            assert!(outcome.reason_message.as_ref().unwrap().is_translatable());
            assert!(
                snapshot.notes.iter().any(|note| note.collector == source && note.message_source.is_some()),
                "human CLI output must also disclose the unqualified source"
            );
        }
        let note_count = snapshot.notes.len();
        record_unreported_optional_sources(&mut snapshot, &options);
        assert_eq!(snapshot.sources, expected, "qualification is recorded only once");
        assert_eq!(snapshot.notes.len(), note_count, "repeated qualification must not duplicate notes");
    }

    #[test]
    fn job_native_failure_assessments_do_not_invent_a_log_gap_or_hide_an_explicit_log_source_failure() {
        use culprit_core::{
            SourceOutcome, SourceState,
            telemetry::{TIMESCALE_JOB_RUNS_SOURCE, TIMESCALE_JOBS_SOURCE},
        };
        let options = SnapshotOptions { collect_native_failures: true, ..Default::default() };
        for state in
            [SourceState::Collected, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable]
        {
            let mut snapshot = recorded();
            for source in [TIMESCALE_JOBS_SOURCE, TIMESCALE_JOB_RUNS_SOURCE] {
                snapshot.sources.push(SourceOutcome {
                    source: source.into(),
                    state,
                    reason: Some("provider job assessment".into()),
                    reason_message: None,
                });
            }
            let original = snapshot.sources.clone();
            record_unreported_optional_sources(&mut snapshot, &options);
            assert_eq!(snapshot.sources, original, "an assessed category cannot gain an unreported log placeholder");
            let explicit = SourceOutcome {
                source: culprit_core::source_id::NATIVE_FAILURES.into(),
                state: SourceState::Unavailable,
                reason: Some("explicit retained-log source failure".into()),
                reason_message: None,
            };
            snapshot.sources.push(explicit.clone());
            record_unreported_optional_sources(&mut snapshot, &options);
            assert_eq!(
                snapshot
                    .sources
                    .iter()
                    .filter(|source| source.source == culprit_core::source_id::NATIVE_FAILURES)
                    .collect::<Vec<_>>(),
                vec![&explicit]
            );
        }
    }

    #[test]
    fn optional_diagnostic_qualification_preserves_provider_outcomes_and_custom_native_sources() {
        use culprit_core::{SourceOutcome, SourceState, telemetry::NativeFailureSnapshot};
        let options = SnapshotOptions { collect_native_failures: true, ..Default::default() };
        for state in [SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable, SourceState::Empty] {
            let mut snapshot = recorded();
            snapshot.sources.push(SourceOutcome {
                source: culprit_core::source_id::NATIVE_FAILURES.into(),
                state,
                reason: Some("provider-owned qualification".into()),
                reason_message: None,
            });
            let original = snapshot.sources.clone();
            record_unreported_optional_sources(&mut snapshot, &options);
            assert_eq!(snapshot.sources, original);
        }
        let mut snapshot = recorded();
        snapshot.operations.native_failures = Some(NativeFailureSnapshot {
            source: "embedding-native-diagnostics".into(),
            observed_from: "2026-11-13T08:00:00Z".into(),
            observed_until: "2026-11-13T08:00:01Z".into(),
            resource: None,
            observations: vec![],
        });
        let original = snapshot.sources.clone();
        record_unreported_optional_sources(&mut snapshot, &options);
        assert_eq!(snapshot.sources, original, "an embedding provider owns its native source qualification");
    }

    #[test]
    fn an_unrelated_source_spelling_cannot_qualify_a_missing_native_observation() {
        use culprit_core::{SourceOutcome, SourceState, source_id};
        let mut snapshot = recorded();
        let unrelated = SourceOutcome {
            source: "SESSION_MEMORY".into(),
            state: SourceState::Collected,
            reason: None,
            reason_message: None,
        };
        snapshot.sources.push(unrelated.clone());
        let options = SnapshotOptions { collect_session_memory: true, ..Default::default() };
        record_unreported_optional_sources(&mut snapshot, &options);
        assert!(snapshot.sources.contains(&unrelated));
        let native = snapshot.sources.iter().find(|outcome| outcome.source == source_id::SESSION_MEMORY).unwrap();
        assert_eq!(native.state, SourceState::Unavailable);
        assert!(native.reason_message.is_some());
    }

    #[test]
    fn public_scan_futures_leave_stack_space_for_native_decoding_and_nested_consumers() {
        const MAX_INLINE_SCAN_BYTES: usize = 4096;
        let root = tempfile::tempdir().unwrap();
        let workspace = crate::Workspace::open_in(root.path());
        let options = SnapshotOptions::default();
        let scan = workspace.scan("unused", &options);
        let with_log = workspace.scan_with_log("unused", &options, None);
        let populations = workspace.scan_populations("unused", &options);
        let verify = workspace.verify("unused", &[]);
        let verify_at = workspace.verify_at("unused", "report", &[]);
        for bytes in [
            std::mem::size_of_val(&scan),
            std::mem::size_of_val(&with_log),
            std::mem::size_of_val(&populations),
            std::mem::size_of_val(&verify),
            std::mem::size_of_val(&verify_at),
        ] {
            assert!(
                bytes <= MAX_INLINE_SCAN_BYTES,
                "scan retains {bytes} inline bytes; observation state belongs on the heap"
            );
        }
        // A boxed future can still create large poll frames when its completed output moves inline through awaits.
        let batch_bytes = std::mem::size_of::<PopulationReports>();
        assert!(batch_bytes <= MAX_INLINE_SCAN_BYTES, "completed report batch transports {batch_bytes} inline bytes");
        assert!(!root.path().join("source-admission").exists());
    }
}
