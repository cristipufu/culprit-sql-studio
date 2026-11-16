//! How a snapshot becomes a report. Four stages, in this order, each a list you can read top to bottom and extend:
//!
//! 1. **Prepare**: make the snapshot say what the database says now, before anything reads it. Query Store keeps a
//!    plan's first XML, so a plan captured before a column changed type still shows the old conversion; marking it
//!    here keeps every check and every plan insight from reporting a mismatch the table no longer has.
//! 2. **Detect**: every check that applies to the engine and can run with what the login sees looks at the snapshot
//!    for one kind of problem ([`crate::all`]). A check that needs a capability the scan lacks is listed as locked,
//!    with the reason, instead of running on missing data.
//! 3. **Refine**: rules over every check's findings (`refine.rs`). Two findings about one cause become one, and a
//!    finding that rests on statements whose cost is behind them (they stopped running, or ran once) says so and ranks
//!    low. A rule here applies to every check on every engine: a false flag several checks could raise is fixed once.
//! 4. **Rank and summarize**: active findings first, by impact, then the headline.
//!
//! Before all of this, a provider's sources (Query Store, pg_stat_statements, the index usage views…) fill the snapshot,
//! and nothing after it touches a connection. An embedding application adds its own checks, preparation steps or
//! rules with [`Pipeline::with_check`], [`Pipeline::with_prepare`] and [`Pipeline::with_refine`]; they run, rank and
//! summarize exactly like the built-in ones.

use std::sync::Arc;

use culprit_core::{Check, CollectorNote, Finding, LockedCheck, Report, Snapshot, t};

mod cleanup;
mod compiler_admission;
mod maintenance_progress;
mod replication;

/// A step that corrects the snapshot before any check reads it. It changes what the data says only where the data
/// contradicts itself or the database (a plan older than the table it describes), never to hide a problem.
pub trait Prepare: Send + Sync {
    /// What it corrects, in a few words.
    fn name(&self) -> &str;
    fn prepare(&self, snapshot: &mut Snapshot);
    fn version(&self) -> Option<&str> {
        None
    }
}

/// A rule over all the findings, after every check ran: merge two findings about one cause, or say that a finding
/// rests on statements that are history.
pub trait Refine: Send + Sync {
    /// What it fixes, in a few words.
    fn name(&self) -> &str;
    fn refine(&self, snapshot: &Snapshot, findings: &mut Vec<Finding>);
    fn version(&self) -> Option<&str> {
        None
    }

    /// Apply the rule and expose its decisions. Existing extensions keep their original implementation; the default
    /// records invocation without pretending to know their policy. A structured rule decides before rendering prose.
    fn refine_traced(&self, snapshot: &Snapshot, findings: &mut Vec<Finding>) -> Vec<culprit_core::RefinementDecision> {
        self.refine(snapshot, findings);
        vec![culprit_core::RefinementDecision {
            rule: self.name().into(),
            finding: None,
            outcome: culprit_core::RefinementOutcome::RuleApplied,
            queries: Vec::new(),
            tables: Vec::new(),
        }]
    }
}

/// A built-in preparation step: a name and a function.
pub(crate) struct Step<F> {
    pub name: &'static str,
    pub run: F,
}

impl Prepare for Step<fn(&mut Snapshot)> {
    fn name(&self) -> &str {
        self.name
    }

    fn prepare(&self, snapshot: &mut Snapshot) {
        (self.run)(snapshot)
    }
}

/// The built-in preparation steps, in the order they run.
fn preparation() -> Vec<Arc<dyn Prepare>> {
    let step = |name: &'static str, run: fn(&mut Snapshot)| Arc::new(Step { name, run }) as Arc<dyn Prepare>;
    vec![
        step("retained observations are worded for this reader", |snapshot| {
            snapshot.render_recorded_messages();
        }),
        step("measurement context identifies this database and its clock", measurement_context),
        step("index availability and maintenance follow the engine", |s| {
            for i in &mut s.indexes {
                i.state = Some(if i.is_disabled {
                    s.server.engine.spec().inactive_index_state(&s.server)
                } else {
                    culprit_core::IndexState::Active
                });
                if i.maintained.is_none() {
                    i.maintained = match i.state {
                        Some(culprit_core::IndexState::Disabled) => Some(false),
                        Some(culprit_core::IndexState::Invalid) => None,
                        _ => Some(true),
                    };
                }
            }
        }),
        step("CTE writes named for their base table", culprit_core::sql::resolve_write_targets),
        step("plans captured before a column changed type", culprit_core::insights::mark_snapshot),
        step("MySQL's possible keys named with their columns", culprit_core::insights::mark_snapshot_keys),
        step("plan operators marked with their index's kind", culprit_core::insights::mark_snapshot_index_kinds),
        step("health counters graded only on the evidence", crate::pressure::grade_on_evidence),
        step("MariaDB's lock time unknown where it counts the whole run", lock_time_that_counts_the_run),
        step("a scan's filter judged by what its runs measured", crate::indexes::mark_measured_kept),
    ]
}

/// A workload is graded on its own period fields; this checks what they add up to. One that doesn't isolate this
/// database, or that names another, is left unassessed; a length nobody can stand behind (bounds the wrong way round, or
/// a window whose bounds and length disagree) is unknown, not a rate's denominator.
fn measurement_context(snapshot: &mut Snapshot) {
    use culprit_core::MeasurementScope;
    let Some(context) = snapshot.identified_measurement() else { return };
    let reason = if context.scope != MeasurementScope::Database {
        Some(t!("pipeline.note.workload_not_isolated"))
    } else if !snapshot.identity_names_this_database() {
        Some(t!("pipeline.note.workload_of_another_database"))
    } else {
        None
    };
    if let Some(message) = reason {
        snapshot.workload = None;
        snapshot.notes.push(CollectorNote { message_source: None, collector: "workload".into(), message, table: None });
        return;
    }
    if let Some(workload) = &mut snapshot.workload {
        workload.window_hours = context.period.hours().unwrap_or(0.0);
    }
}

/// With the slow query log writing to a table, MariaDB reports a logged statement's whole run as its lock time
/// ([`crate::signals::lock_time_counts_the_run`]): 49,963.3 ms of lock in a run of 49,963.9 ms. The real lock time isn't
/// kept, so it is unknown (`None`, and no lock wait), never read as waiting by any check (what a heavy query waited
/// on, how long reads waited on writes). The slow log's own total of lock time is made of such runs too. A note says so.
fn lock_time_that_counts_the_run(snapshot: &mut Snapshot) {
    let Some(workload) = snapshot.workload.as_ref() else { return };
    let logged_runs = workload.kind() == Some(culprit_core::WorkloadKind::SlowLog);
    let inflated: Vec<String> = workload
        .queries
        .iter()
        .filter(|q| crate::signals::lock_time_counts_the_run(snapshot, q))
        .filter(|q| q.observed_wait_ms(culprit_core::telemetry::WaitClass::Lock).is_some())
        .map(|q| q.id.clone())
        .collect();
    if inflated.is_empty() {
        return;
    }
    if let Some(workload) = snapshot.workload.as_mut() {
        for q in workload.queries.iter_mut().filter(|q| inflated.contains(&q.id)) {
            q.invalidate_wait_class(culprit_core::telemetry::WaitClass::Lock);
        }
    }
    if logged_runs {
        snapshot.waits.retain(|w| culprit_core::waits::known_category_key(&w.category) != Some("lock"));
    }
    snapshot.notes.push(CollectorNote {
        message_source: None,
        collector: "workload".into(),
        message: t!("pipeline.note.mariadb_lock_time", n = inflated.len(), count = inflated.len().to_string()),
        table: None,
    });
}

/// The stages and what runs in each.
#[derive(Clone)]
pub struct Pipeline {
    prepare: Vec<Arc<dyn Prepare>>,
    checks: Vec<Arc<dyn Check>>,
    refine: Vec<Arc<dyn Refine>>,
    extensions: Vec<culprit_core::AnalysisExtension>,
}

impl Pipeline {
    /// Culprit's own steps, checks and rules.
    pub fn builtin() -> Self {
        Pipeline {
            prepare: preparation(),
            checks: crate::all().into_iter().map(Arc::from).collect(),
            refine: crate::refine::rules(),
            extensions: Vec::new(),
        }
    }

    /// Adds a check after the built-in ones. A check whose id is already taken is ignored, so a plug-in can't replace
    /// a built-in check's findings.
    pub fn with_check(mut self, check: Arc<dyn Check>) -> Self {
        let id = check.info().id;
        if !self.checks.iter().any(|c| c.info().id == id) {
            self.extensions.push(culprit_core::AnalysisExtension {
                stage: culprit_core::AnalysisStage::Check,
                id,
                version: check.version().map(str::to_string),
            });
            self.checks.push(check);
        }
        self
    }

    /// Adds a preparation step after the built-in ones.
    pub fn with_prepare(mut self, step: Arc<dyn Prepare>) -> Self {
        self.extensions.push(culprit_core::AnalysisExtension {
            stage: culprit_core::AnalysisStage::Prepare,
            id: step.name().into(),
            version: step.version().map(str::to_string),
        });
        self.prepare.push(step);
        self
    }

    /// Adds a rule after the built-in ones, before ranking.
    pub fn with_refine(mut self, rule: Arc<dyn Refine>) -> Self {
        self.extensions.push(culprit_core::AnalysisExtension {
            stage: culprit_core::AnalysisStage::Refine,
            id: rule.name().into(),
            version: rule.version().map(str::to_string),
        });
        self.refine.push(rule);
        self
    }

    /// Every check, in run order.
    pub fn checks(&self) -> &[Arc<dyn Check>] {
        &self.checks
    }

    /// Prepare, detect, refine, rank and summarize.
    pub fn run(&self, mut snapshot: Snapshot) -> Report {
        let evidence = Some(culprit_core::ReportEvidence {
            input: Box::new(snapshot.clone()),
            collected: None,
            recorded_changes: Vec::new(),
        });
        let provenance = Some(culprit_core::AnalysisProvenance {
            analyzer: culprit_core::ComponentVersion::package(
                "culprit-checks",
                env!("CARGO_PKG_VERSION"),
                option_env!("CULPRIT_BUILD_REVISION").or(option_env!("GITHUB_SHA")),
            ),
            pipeline: culprit_core::PipelineProvenance {
                prepare: self.prepare.iter().map(|s| s.name().into()).collect(),
                checks: self.checks.iter().map(|c| c.info().id).collect(),
                refine: self.refine.iter().map(|r| r.name().into()).collect(),
            },
            extensions: self.extensions.clone(),
            origin: culprit_core::AnalysisOrigin::Snapshot,
            collection: None,
            previous_analyzer: None,
            baselines: snapshot.baseline_references(),
        });
        for step in &self.prepare {
            step.prepare(&mut snapshot);
        }
        let memory_failures = explained_memory_failures(&mut snapshot);
        let native_failures = explained_native_failures(&mut snapshot);
        let request_memory = explained_request_memory(&mut snapshot);
        let session_memory = explained_session_memory(&mut snapshot);
        let page_waits = explained_page_waits(&mut snapshot);
        let task_space = explained_task_space(&mut snapshot);
        let query_compilation = explained_compilation(&mut snapshot);
        let compiler_admission = compiler_admission::explain(&mut snapshot);
        let maintenance_progress = maintenance_progress::explain(&mut snapshot);
        let replication = replication::explain(&mut snapshot);
        let cleanup = cleanup::explain(&mut snapshot);
        let mut findings = Vec::new();
        let mut locked = Vec::new();
        let mut assessments = Vec::new();
        for check in &self.checks {
            let info = check.info();
            if !info.engines.contains(&snapshot.server.engine) {
                continue;
            }
            let missing = crate::missing_for(&info, &snapshot.server);
            let mut assessment = culprit_core::CheckAssessment {
                check: info.id.clone(),
                title: info.title.clone(),
                status: culprit_core::CheckStatus::Assessed,
                reason: None,
                assessed_queries: Vec::new(),
            };
            if !missing.is_empty() {
                assessment.status = culprit_core::CheckStatus::Unavailable;
                assessment.reason = Some(
                    missing
                        .iter()
                        .map(|c| {
                            snapshot
                                .server
                                .missing
                                .iter()
                                .find(|m| m.capability == *c)
                                .map(|m| m.reason.clone())
                                .unwrap_or_else(|| t!("pipeline.not_collected", capability = format!("{c:?}")))
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                assessments.push(assessment);
                locked.push(LockedCheck { check: info, missing });
                continue;
            }
            use culprit_core::{CheckEvidence, CheckStatus};
            match check.evidence(&snapshot) {
                CheckEvidence::Available => {}
                CheckEvidence::Unavailable(reason) => {
                    assessment.status = CheckStatus::Unavailable;
                    assessment.reason = Some(reason);
                }
                CheckEvidence::Insufficient(reason) => {
                    assessment.status = CheckStatus::InsufficientEvidence;
                    assessment.reason = Some(reason);
                }
                CheckEvidence::Queries { assessed, reason } => {
                    if reason.is_some() {
                        assessment.status = CheckStatus::InsufficientEvidence;
                    }
                    assessment.reason = reason;
                    assessment.assessed_queries = assessed;
                }
                CheckEvidence::NotApplicable(reason) => {
                    assessment.status = CheckStatus::NotApplicable;
                    assessment.reason = Some(reason);
                }
            }
            let run = !matches!(assessment.status, CheckStatus::Unavailable | CheckStatus::NotApplicable);
            assessments.push(assessment);
            if !run {
                continue;
            }
            findings.extend(check.run(&snapshot));
        }
        let mut refinement = Vec::new();
        for rule in &self.refine {
            refinement.extend(rule.refine_traced(&snapshot, &mut findings));
        }
        for f in &mut findings {
            f.actionability = if f.inactive {
                culprit_core::Actionability::History
            } else if f.actionability == culprit_core::Actionability::OptionalImprovement {
                if f.severity == culprit_core::Severity::Low {
                    culprit_core::Actionability::OptionalImprovement
                } else {
                    culprit_core::Actionability::NeedsAttention
                }
            } else {
                f.actionability
            };
        }
        rank(&mut findings);
        crate::pressure::grade_findings(&mut snapshot, &findings);
        let summary = crate::summarize(&snapshot, &findings);
        crate::annotate_plans(&mut snapshot);
        for assessment in &mut assessments {
            if findings.iter().any(|f| f.check == assessment.check) {
                assessment.status = culprit_core::CheckStatus::Findings;
            }
        }
        Report {
            snapshot,
            findings,
            locked,
            summary,
            assessments,
            refinement,
            provenance,
            evidence,
            memory_failures,
            native_failures,
            request_memory,
            session_memory,
            page_waits,
            task_space,
            query_compilation,
            compiler_admission,
            maintenance_progress,
            replication,
            cleanup,
        }
    }
}

fn explained_memory_failures(snapshot: &mut Snapshot) -> Option<culprit_core::RetainedFailureReport> {
    let captured = snapshot.operations.memory_failures.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = crate::memory_failures::explain(captured, source);
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.memory_failures.invalid_capture"), result)
}

fn explained_native_failures(snapshot: &mut Snapshot) -> Option<culprit_core::RetainedFailureReport> {
    let captured = snapshot.operations.native_failures.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = crate::native_failures::explain(captured, source);
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.native_failures.invalid_capture"), result)
}

fn explained_request_memory(snapshot: &mut Snapshot) -> Option<culprit_core::MemoryParticipantsReport> {
    let captured = snapshot.pressure.as_ref()?.request_memory.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = if captured.is_valid()
        && captured.observations.iter().all(|row| row.fits_database_capture(&snapshot.server.database))
    {
        let rows: Vec<_> = captured.observations.iter().collect();
        crate::request_memory::explain(
            crate::request_memory::RequestMemoryEvidence { context: &captured.context, observations: &rows },
            source,
        )
    } else {
        Err(culprit_core::Error::invalid(culprit_core::t!("view.request_memory.invalid_capture")))
    };
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.request_memory.invalid_capture"), result)
}

fn explained_session_memory(snapshot: &mut Snapshot) -> Option<culprit_core::MemoryParticipantsReport> {
    let captured = snapshot.operations.session_memory.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = if captured.is_valid() {
        let rows: Vec<_> = captured.observations.iter().collect();
        crate::session_memory::explain(&rows, source)
    } else {
        Err(culprit_core::Error::invalid(culprit_core::t!("view.session_memory.invalid_capture")))
    };
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.session_memory.invalid_capture"), result)
}

fn explained_page_waits(snapshot: &mut Snapshot) -> Option<culprit_core::PageWaitReport> {
    let captured = snapshot.operations.page_waits.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = if captured.is_valid()
        && captured.observations.iter().all(|row| row.fits_database_capture(&snapshot.server.database))
    {
        let rows: Vec<_> = captured.observations.iter().collect();
        crate::page_waits::explain(&rows, source, None)
    } else {
        Err(culprit_core::Error::invalid(culprit_core::t!("view.page_waits.invalid_capture")))
    };
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.page_waits.invalid_capture"), result)
}

fn explained_task_space(snapshot: &mut Snapshot) -> Option<culprit_core::TaskSpaceReport> {
    let captured = snapshot.operations.task_space.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = if captured.is_valid()
        && captured.observations.iter().all(|row| row.fits_database_capture(&snapshot.server.database))
    {
        let rows: Vec<_> = captured.observations.iter().collect();
        crate::task_space::explain(&rows, source)
    } else {
        Err(culprit_core::Error::invalid(culprit_core::t!("view.task_space.invalid_capture")))
    };
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.task_space.invalid_capture"), result)
}

fn explained_compilation(snapshot: &mut Snapshot) -> Option<culprit_core::CompilationReport> {
    let captured = snapshot.operations.query_compilation.as_ref()?;
    let mut source = snapshot.sources.iter().find(|source| source.source == captured.source).cloned();
    // The source can also expose database-wide totals. The validated point independently establishes its own
    // selected row set, including an empty one; a failed or partial read is never upgraded by that projection.
    if captured.observations.is_empty()
        && let Some(source) = source.as_mut().filter(|source| source.state == culprit_core::SourceState::Collected)
    {
        source.state = culprit_core::SourceState::Empty;
    }
    let result = if captured.is_valid() {
        let rows: Vec<_> = captured.observations.iter().collect();
        crate::compilation::explain(&rows, source.as_ref())
    } else {
        Err(culprit_core::Error::invalid(culprit_core::t!("view.compilation.invalid")))
    };
    let source_id = captured.source.clone();
    admit_explanation(snapshot, source_id, culprit_core::message!("view.compilation.invalid"), result)
}

fn admit_explanation<R>(
    snapshot: &mut Snapshot,
    source_id: String,
    message: culprit_core::i18n::Message,
    explained: culprit_core::Result<R>,
) -> Option<R> {
    match explained {
        Ok(report) => Some(report),
        Err(_) => {
            if let Some(source) = snapshot.sources.iter_mut().find(|source| source.source == source_id) {
                source.state = culprit_core::SourceState::Unavailable;
                source.reason = Some(message.render());
                source.reason_message = Some(message.clone());
            } else {
                snapshot.sources.push(culprit_core::SourceOutcome {
                    source: source_id.clone(),
                    state: culprit_core::SourceState::Unavailable,
                    reason: Some(message.render()),
                    reason_message: Some(message.clone()),
                });
            }
            snapshot.notes.push(CollectorNote {
                message_source: Some(message.clone()),
                collector: source_id,
                message: message.render(),
                table: None,
            });
            None
        }
    }
}

/// Active findings first, then by severity, then by impact. Severity already weighs a finding's share of database time
/// with the time behind it: a Low finding with a large share of a quiet database's few milliseconds (or a warning the
/// plan says is harmless) never tops a Medium one.
fn rank(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.actionability
            .cmp(&b.actionability)
            .then(a.inactive.cmp(&b.inactive))
            .then(b.severity.cmp(&a.severity))
            .then(b.impact.score.total_cmp(&a.impact.score))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Capability, Engine, WaitStat};

    fn compilation_snapshot() -> Snapshot {
        use culprit_core::{CompilationMemory, ResetMarker, SourceOutcome, SourceState, telemetry::*};
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
        let source = "embedded.compilation-observations";
        let native = MssqlQueryCompilation {
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            origin: StatementOrigin::Unknown,
            identity: NativeQueryIdentity {
                query_id: "42".into(),
                qualifiers: [
                    ("context_settings_id".into(), "5".into()),
                    ("object_id".into(), "1999".into()),
                    ("query_hash".into(), "0123456789ABCDEF".into()),
                    ("initial_compile_at".into(), "2020-01-01T06:00:00.1533333Z".into()),
                ]
                .into(),
            },
            compilations: "9007199254740993".into(),
            last_compile_at: "2020-01-01T11:59:00.1533333Z".into(),
            average_duration_us: 1500.5,
            memory: Some(CompilationMemory { average_kb: Some(2048.5), last_kb: Some(0.0), maximum_kb: Some(16384.0) }),
            seconds_since_first_compile: 21600,
            reset: ResetMarker::Unknown,
        };
        snapshot.operations.query_compilation = Some(CompilationSnapshot {
            source: source.into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: vec![OperationalObservation {
                source: source.into(),
                key: native.key().unwrap(),
                fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
            }],
        });
        snapshot.sources.push(SourceOutcome {
            source: source.into(),
            state: SourceState::Collected,
            reason: Some("independently retained database compilation totals".into()),
            reason_message: None,
        });
        snapshot
    }

    #[test]
    fn compilation_replay_uses_the_same_pure_report_and_keeps_native_lifetimes_outside_read_bounds() {
        use culprit_core::telemetry::ObservationBounds;
        let snapshot = compilation_snapshot();
        let native = snapshot.operations.query_compilation.as_ref().unwrap();
        let source = snapshot.sources.iter().find(|source| source.source == native.source).unwrap();
        assert!(native.is_valid());
        let point = ObservationBounds::new(&native.observed_from, &native.observed_until).unwrap();
        let culprit_core::telemetry::OperationalFact::MssqlQueryCompilation(compilation) = &native.observations[0].fact
        else {
            panic!()
        };
        assert!(!point.contains(&compilation.first_compile_at().unwrap()));
        assert!(!point.contains(&compilation.last_compile_at));
        let expected =
            crate::compilation::explain(&native.observations.iter().collect::<Vec<_>>(), Some(source)).unwrap();
        let original = native.clone();
        let report = crate::analyze(snapshot);
        assert_eq!(
            serde_json::to_value(report.query_compilation.as_ref().unwrap()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(report.snapshot.operations.query_compilation.as_ref(), Some(&original));
        let entry = &report.query_compilation.as_ref().unwrap().entries[0];
        assert!(
            entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.count")
                && metric.text.as_deref() == Some("9007199254740993"))
        );
        assert!(entry.metrics.iter().any(|metric| metric.is("common.metric.compile_memory_last") && metric.text.as_deref() == Some("0 KiB")));
    }

    #[test]
    fn a_validated_empty_compilation_point_does_not_rewrite_collected_global_totals() {
        use culprit_core::SourceState;
        let mut snapshot = compilation_snapshot();
        snapshot.operations.query_compilation.as_mut().unwrap().observations.clear();
        let native = snapshot.operations.query_compilation.clone().unwrap();
        let original_source = snapshot.sources.iter().find(|source| source.source == native.source).cloned().unwrap();
        let report = crate::analyze(snapshot);
        let explanation = report.query_compilation.as_ref().unwrap();
        assert_eq!(explanation.source_state, Some(SourceState::Empty));
        assert!(explanation.entries.is_empty());
        assert_eq!(explanation.source_reason, original_source.reason);
        assert_eq!(
            report.snapshot.sources.iter().find(|source| source.source == native.source),
            Some(&original_source)
        );
        assert_eq!(report.snapshot.operations.query_compilation, Some(native));
    }

    #[test]
    fn partial_unavailable_and_unknown_compilation_visibility_are_not_upgraded_by_replay() {
        use culprit_core::SourceState;
        for (state, empty) in [
            (Some(SourceState::Partial), false),
            (Some(SourceState::Partial), true),
            (Some(SourceState::Unavailable), true),
            (None, false),
        ] {
            let mut snapshot = compilation_snapshot();
            let native = snapshot.operations.query_compilation.as_mut().unwrap();
            if empty {
                native.observations.clear();
            }
            let source_name = native.source.clone();
            if let Some(state) = state {
                snapshot.sources.iter_mut().find(|source| source.source == source_name).unwrap().state = state;
            } else {
                snapshot.sources.retain(|source| source.source != source_name);
            }
            let original = snapshot.operations.query_compilation.clone();
            let report = crate::analyze(snapshot);
            let explanation = report.query_compilation.as_ref().unwrap();
            assert_eq!(explanation.source_state, state);
            assert_eq!(explanation.entries.is_empty(), empty);
            assert_eq!(
                report.snapshot.sources.iter().find(|source| source.source == source_name).map(|source| source.state),
                state
            );
            assert_eq!(report.snapshot.operations.query_compilation, original);
        }
    }

    #[test]
    fn invalid_compilation_clocks_duplicate_keys_and_foreign_native_keys_withhold_only_the_explanation() {
        use culprit_core::{SourceState, telemetry::*};
        for defect in ["clock", "duplicate", "native-key"] {
            let mut snapshot = compilation_snapshot();
            let native = snapshot.operations.query_compilation.as_mut().unwrap();
            match defect {
                "clock" => native.observed_until = "2026-11-13T09:59:59Z".into(),
                "duplicate" => {
                    let mut repeated = native.observations[0].clone();
                    let OperationalFact::MssqlQueryCompilation(compilation) = &mut repeated.fact else { panic!() };
                    compilation.compilations = "9007199254740994".into();
                    assert!(repeated.is_valid());
                    native.observations.push(repeated);
                }
                "native-key" => native.observations[0].key = digest(b"different native compilation identity"),
                _ => unreachable!(),
            }
            let original = native.clone();
            let report = crate::analyze(snapshot);
            assert!(report.query_compilation.is_none(), "{defect}");
            assert_eq!(report.snapshot.operations.query_compilation.as_ref(), Some(&original), "{defect}");
            let failed = report.snapshot.sources.iter().find(|source| source.source == original.source).unwrap();
            assert_eq!(failed.state, SourceState::Unavailable);
            assert_eq!(failed.reason.as_deref(), Some(t!("view.compilation.invalid").as_str()));
            assert!(report.snapshot.notes.iter().any(|note| note.collector == original.source));
        }
    }

    #[test]
    fn task_space_replay_keeps_empty_custom_source_coverage_and_withholds_invalid_clocks() {
        use culprit_core::{SourceOutcome, SourceState, telemetry::TaskSpaceSnapshot};
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
        let source = "embedded.task-observations";
        let capture = TaskSpaceSnapshot {
            source: source.into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: Vec::new(),
        };
        snapshot.operations.task_space = Some(capture.clone());
        snapshot.sources.push(SourceOutcome {
            source: source.into(),
            state: SourceState::Empty,
            reason: Some("native task read was empty".into()),
            reason_message: None,
        });
        let report = crate::analyze(snapshot.clone());
        let explanation = report.task_space.as_ref().unwrap();
        assert_eq!(explanation.source_state, Some(SourceState::Empty));
        assert_eq!(explanation.source_reason.as_deref(), Some("native task read was empty"));
        assert!(explanation.entries.is_empty());
        assert_eq!(report.snapshot.operations.task_space, Some(capture));

        snapshot.operations.task_space.as_mut().unwrap().observed_until = "2026-11-13T09:59:59Z".into();
        let report = crate::analyze(snapshot);
        assert!(report.task_space.is_none());
        let unavailable = report.snapshot.sources.iter().find(|outcome| outcome.source == source).unwrap();
        assert_eq!(unavailable.state, SourceState::Unavailable);
        assert_eq!(unavailable.reason.as_deref(), Some(t!("view.task_space.invalid_capture").as_str()));
        assert!(report.snapshot.operations.task_space.is_some(), "invalid native input remains available");
        assert!(report.snapshot.notes.iter().any(|note| note.collector == source));
    }

    #[test]
    fn preparation_preserves_worker_io_totals_above_or_below_statement_elapsed_time() {
        let original: Snapshot = serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        for engine in Engine::ALL {
            for ratio in [0.5, 2.0] {
                let mut s = original.clone();
                s.server.engine = engine;
                s.server.capabilities.push(Capability::WaitStats);
                s.notes.clear();
                let total = s.workload.as_ref().unwrap().total_time_ms;
                s.waits =
                    vec![WaitStat { category: "IO".into(), wait_type: None, wait_ms: total * ratio, tasks: None }];
                for step in preparation() {
                    step.prepare(&mut s);
                }
                assert_eq!(s.waits.len(), 1, "{engine:?}, ratio={ratio}");
                assert_eq!(s.waits[0].wait_ms, total * ratio);
                assert!(s.server.has(Capability::WaitStats));
                assert!(s.waits.iter().all(|wait| wait.category != "Other"));
            }
        }
    }

    #[test]
    fn page_wait_replay_preserves_native_coverage_and_withholds_invalid_capture_explanations() {
        use culprit_core::{SourceOutcome, SourceState, telemetry::PageWaitSnapshot};
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
        let source = "embedded.page-observations";
        let capture = PageWaitSnapshot {
            source: source.into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: Vec::new(),
        };
        snapshot.operations.page_waits = Some(capture.clone());
        snapshot.sources.push(SourceOutcome {
            source: source.into(),
            state: SourceState::Partial,
            reason: Some("bounded coordinator observations".into()),
            reason_message: None,
        });
        let report = crate::analyze(snapshot.clone());
        let explanation = report.page_waits.as_ref().unwrap();
        assert_eq!(explanation.source_state, Some(SourceState::Partial));
        assert_eq!(explanation.source_reason.as_deref(), Some("bounded coordinator observations"));
        assert!(explanation.entries.is_empty());
        assert_eq!(report.snapshot.operations.page_waits, Some(capture));

        snapshot.operations.page_waits.as_mut().unwrap().observed_until = "2026-11-13T09:59:59Z".into();
        let report = crate::analyze(snapshot);
        assert!(report.page_waits.is_none());
        let unavailable = report.snapshot.sources.iter().find(|outcome| outcome.source == source).unwrap();
        assert_eq!(unavailable.state, SourceState::Unavailable);
        assert_eq!(unavailable.reason.as_deref(), Some(t!("view.page_waits.invalid_capture").as_str()));
        assert!(report.snapshot.notes.iter().any(|note| note.collector == source));
        assert!(report.snapshot.operations.page_waits.is_some(), "invalid input remains available for diagnosis");
    }

    #[test]
    fn database_reports_reject_conflicting_claimed_membership_but_keep_unowned_native_context() {
        use culprit_core::{SourceOutcome, SourceState, WaitResourceKind, telemetry::*};
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
        let mut grant = crate::views::request_memory::tests::observation("61", ExecutionGrantState::Granted);
        crate::views::request_memory::tests::associated(&mut grant);
        grant.request.as_mut().unwrap().database = Some(snapshot.server.database.clone());
        snapshot.pressure.get_or_insert_default().request_memory = Some(RequestMemorySnapshot {
            context: NativeReadContext {
                target: culprit_core::DatabaseIdentity::server(
                    snapshot.server.engine,
                    &snapshot.server.database,
                    "fixture.invalid",
                    1433,
                    None,
                )
                .into(),
                partition: SourcePartition::InstanceLocal { member: None, epoch: None },
                observed_from: grant.observed_at.clone(),
                observed_until: grant.observed_at.clone(),
                database_writable: None,
            },
            source: grant.source.clone(),
            observed_at: grant.observed_at.clone(),
            observations: vec![grant.clone()],
        });
        let mut page = crate::views::page_waits::tests::observation("61", WaitResourceKind::MemoryPage);
        page.request.database = Some(snapshot.server.database.clone());
        snapshot.operations.page_waits = Some(PageWaitSnapshot {
            source: page.source.clone(),
            observed_from: page.observed_from.clone(),
            observed_until: page.observed_until.clone(),
            observations: vec![page.clone()],
        });
        for source in [&grant.source, &page.source] {
            snapshot.sources.push(SourceOutcome {
                source: source.clone(),
                state: SourceState::Partial,
                reason: None,
                reason_message: None,
            });
        }
        let valid = crate::analyze(snapshot.clone());
        assert!(valid.request_memory.is_some() && valid.page_waits.is_some());

        let mut foreign = snapshot.clone();
        foreign.pressure.as_mut().unwrap().request_memory.as_mut().unwrap().observations[0]
            .request
            .as_mut()
            .unwrap()
            .database = Some("foreign-database-canary".into());
        foreign.operations.page_waits.as_mut().unwrap().observations[0].request.database =
            Some("foreign-database-canary".into());
        let rejected = crate::analyze(foreign);
        assert!(rejected.request_memory.is_none() && rejected.page_waits.is_none());
        for source in [&grant.source, &page.source] {
            assert_eq!(
                rejected.snapshot.sources.iter().find(|outcome| &outcome.source == source).unwrap().state,
                SourceState::Unavailable
            );
        }
        assert_eq!(
            rejected.snapshot.operations.page_waits.unwrap().observations[0].request.database.as_deref(),
            Some("foreign-database-canary")
        );

        let grant = &mut snapshot.pressure.as_mut().unwrap().request_memory.as_mut().unwrap().observations[0];
        grant.request = None;
        grant.scope = None;
        grant.request_association = RequestGrantAssociation::InsufficientEvidence;
        grant.statement_relationship = GrantStatementRelationship::Unresolved;
        let page = &mut snapshot.operations.page_waits.as_mut().unwrap().observations[0];
        page.request.scope = ActivityScopeBasis::OtherDatabase;
        page.request.database = Some("foreign-database-canary".into());
        page.target = None;
        page.statement_identity = None;
        page.plan_identity = None;
        page.resolution = PageWaitResolution::OutsideDatabase;
        let unowned = crate::analyze(snapshot);
        assert!(unowned.request_memory.is_some() && unowned.page_waits.is_some());
        assert_eq!(unowned.page_waits.unwrap().entries.len(), 1);
    }
}
