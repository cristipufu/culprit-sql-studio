//! Query relationships come from recorded statement and plan identities, independently of their description.
use super::*;
use culprit_core::{FindingPlanChange, RefinementDecision};

#[derive(Clone, Copy)]
pub(super) enum Policy {
    PlanStory,
    ScanRegression,
}
impl Policy {
    fn id(self) -> &'static str {
        match self {
            Self::PlanStory => "one-plan-story",
            Self::ScanRegression => "scan-carries-regression",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::PlanStory => "one plan story per query",
            Self::ScanRegression => "a scan that came with a plan change carries the regression",
        }
    }
}
pub(super) struct Rule(pub Policy);
enum Action {
    Timeline,
    Recommendation,
    Scan(ReadChange),
}
struct Decision {
    from: usize,
    into: usize,
    action: Action,
}
struct ReadChange {
    from: String,
    to: String,
    before: f64,
    after: f64,
}
impl ReadChange {
    fn recorded(q: &QueryStat, change: &FindingPlanChange) -> Option<Self> {
        if change.from == change.to || q.plan_id.as_deref().is_some_and(|id| id != change.to) {
            return None;
        }
        let per_run = |id: &str| {
            let p = q.workload_plans().iter().find(|p| p.id == id)?;
            let reads = p.logical_reads?;
            (p.executions.is_finite() && p.executions > 0.0 && reads.is_finite() && reads >= 0.0)
                .then_some(reads / p.executions)
        };
        let (before, after) = (per_run(&change.from)?, per_run(&change.to)?);
        (after > before * 2.0).then(|| Self { from: change.from.clone(), to: change.to.clone(), before, after })
    }
    fn render(&self, history: bool) -> String {
        t!(
            "refine.queries.scan_came_with_plan_change",
            from = &self.from,
            before = format::count(self.before),
            to = &self.to,
            history = if history { "yes" } else { "no" },
            after = format::count(self.after)
        )
    }
}
fn single(f: &Finding) -> Option<&str> {
    match f.queries.as_slice() {
        [r] => Some(&r.id),
        _ => None,
    }
}
fn decide(policy: Policy, s: &Snapshot, findings: &[Finding]) -> Option<Decision> {
    let w = s.workload.as_ref()?;
    // Timeline evidence joins the engine's recommendation before that recommendation joins a sensitivity finding.
    if matches!(policy, Policy::PlanStory) {
        for (into, rec) in findings
            .iter()
            .enumerate()
            .filter(|(_, f)| f.check == "tuning-advice" && f.facts.recommended_plan.is_some())
        {
            let Some(id) = single(rec).filter(|id| w.query(id).is_some()) else { continue };
            if let Some(from) = findings
                .iter()
                .position(|f| matches!(f.check.as_str(), "regression" | "plan-regression") && single(f) == Some(id))
            {
                return Some(Decision { from, into, action: Action::Timeline });
            }
            if let Some(target) =
                findings.iter().position(|f| f.check == "parameter-sensitive" && single(f) == Some(id))
            {
                return Some(Decision { from: into, into: target, action: Action::Recommendation });
            }
        }
        return None;
    }
    for (from, f) in findings.iter().enumerate() {
        let Some(q) = single(f).and_then(|id| w.query(id)) else { continue };
        match policy {
            Policy::ScanRegression if matches!(f.check.as_str(), "regression" | "plan-regression") => {
                let Some(reads) = f.facts.plan_change.as_ref().and_then(|change| ReadChange::recorded(q, change))
                else {
                    continue;
                };
                if let Some(into) = findings.iter().position(|g| {
                    matches!(g.check.as_str(), "filtered-scan" | "missing-index")
                        && g.queries.iter().any(|r| r.id == q.id && r.plan_node.is_some())
                }) {
                    return Some(Decision { from, into, action: Action::Scan(reads) });
                }
            }
            _ => {}
        }
    }
    None
}
fn add_metrics(target: &mut Finding, source: &Finding) {
    for m in &source.metrics {
        let already_present = !m.key.is_empty() && target.metrics.iter().any(|metric| metric.is(&m.key));
        if !already_present {
            target.metrics.push(m.clone());
        }
    }
}
/// The step in the runs a regression finding (`source`) tells, as the finding that takes it in says it ("Query Store's
/// timeline: until …"): naming the statement where that finding is about several (`named`).
fn timeline(s: &Snapshot, source: &Finding, named: bool) -> Option<String> {
    let query = single(source)?;
    match source.check.as_str() {
        "regression" => crate::queries::regression_timeline(s, query),
        "plan-regression" => crate::queries::plan_regression_timeline(s, query, named),
        _ => None,
    }
}
fn recommendation_caveat(rec: &Finding, sensitive: &Finding) -> String {
    let recommended = rec.facts.recommended_plan.as_ref().map(|r| r.plan.as_str());
    let saving = rec.facts.recommended_plan.as_ref().and_then(|r| r.cpu_saved_share);
    let (kind, plan) = recommended.map_or(("unknown", ""), |plan| ("known", plan));
    let share = saving.map(format::percent).unwrap_or_default();
    let saving = if saving.is_some() { "yes" } else { "no" };
    match sensitive.facts.usual_fast_plan.as_deref() {
        Some(fast) if Some(fast) == recommended => {
            t!("refine.queries.force_the_fast_plan", kind = kind, plan = plan, saving = saving, share = share)
        }
        Some(fast) => t!(
            "refine.queries.force_another_than_the_fast_plan",
            kind = kind,
            plan = plan,
            saving = saving,
            share = share,
            fast = fast
        ),
        None => t!("refine.queries.force_plan", kind = kind, plan = plan, saving = saving, share = share),
    }
}
impl Refine for Rule {
    fn name(&self) -> &str {
        self.0.label()
    }
    fn refine(&self, s: &Snapshot, findings: &mut Vec<Finding>) {
        let _ = self.refine_traced(s, findings);
    }
    fn refine_traced(&self, s: &Snapshot, findings: &mut Vec<Finding>) -> Vec<RefinementDecision> {
        let mut trace = Vec::new();
        while let Some(d) = decide(self.0, s, findings) {
            trace.push(merge::trace(self.0.id(), findings, d.from, d.into));
            let source = findings[d.from].clone();
            let absorption =
                matches!(d.action, Action::Scan(_)).then(|| merge::Absorption::decide(s, findings, d.into, d.from));
            if let Some(a) = &absorption {
                a.apply_state(findings);
            } else {
                let target = &mut findings[d.into];
                target.severity = target.severity.max(source.severity);
                target.impact.score = target.impact.score.max(source.impact.score);
                if matches!(d.action, Action::Timeline) {
                    target.impact.time_ms_per_day = target.impact.time_ms_per_day.or(source.impact.time_ms_per_day);
                }
            }
            let target = &mut findings[d.into];
            match d.action {
                Action::Timeline => {
                    if let Some(sentence) = timeline(s, &source, false) {
                        target.summary = format!("{} {sentence}", target.summary.trim_end());
                    }
                    add_metrics(target, &source);
                    for line in source.verify {
                        if !target.verify.contains(&line) {
                            target.verify.push(line);
                        }
                    }
                    for line in source.caveats {
                        if !target.caveats.contains(&line) {
                            target.caveats.push(line);
                        }
                    }
                }
                Action::Recommendation => target.caveats.push(recommendation_caveat(&source, target)),
                Action::Scan(reads) => {
                    let Some(q) = source.queries.first().and_then(|r| s.workload.as_ref()?.query(&r.id)) else {
                        continue;
                    };
                    let sentence = timeline(s, &source, target.queries.len() > 1);
                    target.summary = [target.summary.trim_end()]
                        .into_iter()
                        .chain(sentence.as_deref())
                        .chain([reads.render(query_is_history(s, q)).as_str()])
                        .collect::<Vec<_>>()
                        .join(" ");
                    add_metrics(target, &source);
                }
            }
            if let Some(a) = absorption {
                a.render(findings, None);
                a.finish(findings);
            } else {
                findings.remove(d.from);
            }
        }
        trace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Category, Metric, Unit};

    #[test]
    fn merged_metrics_keep_distinct_keys_even_when_their_labels_match() {
        let subject = support::subject(SubjectKind::Query, "q1");
        let mut target = support::finding("temp-spill", "q1", Category::Queries, subject.clone());
        target.metrics = vec![Metric::new("Same label", 1.0, Unit::Count).keyed("first")];
        let mut source = support::finding("big-result", "q1", Category::Queries, subject);
        source.metrics = vec![
            Metric::new("Same label", 2.0, Unit::Count).keyed("second"),
            Metric::new("Renamed label", 3.0, Unit::Count).keyed("first"),
            Metric::new("Same label", 4.0, Unit::Count),
        ];
        add_metrics(&mut target, &source);
        assert_eq!(target.metrics.iter().map(|metric| metric.value).collect::<Vec<_>>(), [1.0, 2.0, 4.0]);
    }

    // Metrics keep their declared identity independently of their translated labels.
    #[test]
    fn translated_metrics_keep_their_units_and_native_keys() {
        culprit_core::i18n::scope("fr", || {
            let subject = support::subject(SubjectKind::Query, "q1");
            let mut source = support::finding("big-result", "q1", Category::Queries, subject.clone());
            source.metrics = vec![
                Metric::said(key!("common.metric.runs"), 30.0, Unit::Count),
                Metric::new("Lignes par exécution", 5.0, Unit::Rows),
            ];
            assert_ne!(source.metrics[0].label, "Runs");
            let mut target = support::finding("temp-spill", "q1", Category::Queries, subject);
            add_metrics(&mut target, &source);
            assert_eq!(target.metrics.len(), 2);
            assert!(target.metrics[0].is(key!("common.metric.runs")));
            assert_eq!(target.metrics[0].unit, Unit::Count);
            assert_eq!(target.metrics[1].label, "Lignes par exécution");
        });
    }
}
