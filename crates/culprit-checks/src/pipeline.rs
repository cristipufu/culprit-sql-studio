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
        }
    }
}

fn explained_memory_failures(snapshot: &mut Snapshot) -> Option<culprit_core::RetainedFailureReport> {
    let captured = snapshot.operations.memory_failures.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = crate::memory_failures::explain(captured, source);
    let source_id = captured.source.clone();
    admit_retained_explanation(
        snapshot,
        source_id,
        culprit_core::message!("view.memory_failures.invalid_capture"),
        result,
    )
}

fn explained_native_failures(snapshot: &mut Snapshot) -> Option<culprit_core::RetainedFailureReport> {
    let captured = snapshot.operations.native_failures.as_ref()?;
    let source = snapshot.sources.iter().find(|source| source.source == captured.source);
    let result = crate::native_failures::explain(captured, source);
    let source_id = captured.source.clone();
    admit_retained_explanation(
        snapshot,
        source_id,
        culprit_core::message!("view.native_failures.invalid_capture"),
        result,
    )
}

fn admit_retained_explanation(
    snapshot: &mut Snapshot,
    source_id: String,
    message: culprit_core::i18n::Message,
    explained: culprit_core::Result<culprit_core::RetainedFailureReport>,
) -> Option<culprit_core::RetainedFailureReport> {
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
}
