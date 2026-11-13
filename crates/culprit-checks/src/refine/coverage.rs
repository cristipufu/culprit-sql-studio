//! A finding is covered only by an identified cause. Select relationships without mutating findings or reading prose.
use super::*;
use culprit_core::{NodeKind, PlanKind, PlanWarning, QueryRef, RefinementDecision, RefinementOutcome};

#[derive(Clone, Copy)]
pub(super) enum Policy {
    Rewrite,
    SpillSort,
    ForeignKey,
    Regression,
    Heavy,
}
impl Policy {
    fn id(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite-covers-scan",
            Self::SpillSort => "spill-covers-sort",
            Self::ForeignKey => "index-covers-foreign-key",
            Self::Regression => "regression-covers-plan-change",
            Self::Heavy => "cause-covers-heavy-query",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Rewrite => "a rewrite, not an index, fixes a scan behind a function or wildcard",
            Self::SpillSort => "a spill explains the big sort",
            Self::ForeignKey => "an index to add covers the foreign key",
            Self::Regression => "a regression carries its plan change",
            Self::Heavy => "a heavy query another finding explains",
        }
    }
}

pub(super) struct Rule(pub Policy);
impl Refine for Rule {
    fn name(&self) -> &str {
        self.0.label()
    }
    fn refine(&self, s: &Snapshot, findings: &mut Vec<Finding>) {
        let _ = self.refine_traced(s, findings);
    }
    fn refine_traced(&self, s: &Snapshot, findings: &mut Vec<Finding>) -> Vec<RefinementDecision> {
        let decisions = decide(self.0, s, findings);
        let trace = decisions
            .iter()
            .map(|d| RefinementDecision {
                rule: self.0.id().into(),
                finding: Some(findings[d.at].id.clone()),
                outcome: RefinementOutcome::Covered { by: d.by.iter().map(|&at| findings[at].id.clone()).collect() },
                queries: findings[d.at].queries.iter().map(|r| r.id.clone()).collect(),
                tables: if findings[d.at].subject.kind == SubjectKind::Table {
                    vec![findings[d.at].subject.name.clone()]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let gone: BTreeSet<_> = decisions.iter().map(|d| d.at).collect();
        let mut at = 0;
        findings.retain(|_| {
            let keep = !gone.contains(&at);
            at += 1;
            keep
        });
        trace
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Decision {
    at: usize,
    by: Vec<usize>,
}

fn shares_query(a: &Finding, b: &Finding) -> bool {
    a.queries.iter().any(|a| b.queries.iter().any(|b| a.id == b.id))
}

fn spill_covering_sort(s: &Snapshot, sort: &Finding, findings: &[Finding]) -> Vec<usize> {
    if sort.queries.is_empty() {
        return Vec::new();
    }
    let mut covering = BTreeSet::new();
    for reference in &sort.queries {
        if !selected_spilling_sort(s, reference) {
            return Vec::new();
        }
        let matching = findings
            .iter()
            .enumerate()
            .filter(|(_, finding)| {
                finding.facts.cause == Some(FindingCause::Spill)
                    && finding
                        .queries
                        .iter()
                        .any(|spill| spill.id == reference.id && spill.plan_node == reference.plan_node)
            })
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Vec::new();
        }
        covering.extend(matching);
    }
    covering.into_iter().collect()
}

/// Native node IDs are local to the selected plan; a query-wide spill or an estimated operator is not this proof.
fn selected_spilling_sort(s: &Snapshot, reference: &QueryRef) -> bool {
    let Some(node_id) = reference.plan_node else { return false };
    let Some(plan) =
        s.workload.as_ref().and_then(|workload| workload.query(&reference.id)).and_then(|query| query.plan.as_ref())
    else {
        return false;
    };
    if plan.kind != PlanKind::Actual {
        return false;
    }
    let nodes = plan.nodes();
    let mut matching = nodes.iter().filter(|node| node.id == node_id);
    let Some(node) = matching.next() else { return false };
    matching.next().is_none() && node.kind == NodeKind::Sort && node.warnings.contains(&PlanWarning::Spill)
}

fn decide(policy: Policy, s: &Snapshot, findings: &[Finding]) -> Vec<Decision> {
    let heavy = if matches!(policy, Policy::Heavy) { heavy_causes(s, findings) } else { BTreeMap::new() };
    let mut out = Vec::new();
    for (at, f) in findings.iter().enumerate() {
        let by: Vec<_> = match policy {
            Policy::Rewrite if f.check == "filtered-scan" => findings
                .iter()
                .enumerate()
                .filter(|(_, g)| {
                    g.facts.cause == Some(FindingCause::PredicateRewrite)
                        && table_leaf(&g.subject.name) == table_leaf(&f.subject.name)
                        && shares_query(f, g)
                })
                .map(|(at, _)| at)
                .collect(),
            Policy::SpillSort if f.facts.cause == Some(FindingCause::Sort) => spill_covering_sort(s, f, findings),
            Policy::ForeignKey if f.check == "unindexed-fk" => {
                let cols = &f.facts.index_keys;
                if cols.is_empty() {
                    continue;
                }
                findings
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| {
                        if !matches!(g.check.as_str(), "missing-index" | "filtered-scan")
                            || !g.subject.name.eq_ignore_ascii_case(&f.subject.name)
                        {
                            return false;
                        }
                        // Use the same eligibility decision as index-work. A candidate whose SQL will be withheld cannot
                        // promise to cover a foreign key; this is independent of the relative order of these rules.
                        let eligible = lifecycle::recommends_unfiltered_index(s, g);
                        let keys = &g.facts.index_keys;
                        eligible && cols.iter().all(|c| keys.iter().take(cols.len()).any(|k| k.eq_ignore_ascii_case(c)))
                    })
                    .map(|(at, _)| at)
                    .collect()
            }
            Policy::Regression if f.check == "plan-regression" => findings
                .iter()
                .enumerate()
                .filter(|(_, g)| g.check == "regression" && shares_query(f, g))
                .map(|(at, _)| at)
                .collect(),
            Policy::Heavy if f.check == "heavy-query" => {
                let Some(own) = f.impact.db_time_share else {
                    continue;
                };
                f.queries
                    .iter()
                    .flat_map(|r| heavy.get(&r.id).into_iter().flatten())
                    .filter(|(_, weight, part)| *weight >= f.severity && *part >= own * 0.5)
                    .map(|(at, _, _)| *at)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        };
        if !by.is_empty() {
            out.push(Decision { at, by });
        }
    }
    out
}

fn heavy_causes(s: &Snapshot, findings: &[Finding]) -> BTreeMap<String, Vec<(usize, Severity, f64)>> {
    let own_share = |id: &str| s.workload.as_ref().and_then(|w| w.query(id).map(|q| w.statement_share(q)));
    let mut causes: BTreeMap<String, Vec<(usize, Severity, f64)>> = BTreeMap::new();
    for (at, f) in findings.iter().enumerate().filter(|(_, f)| {
        !matches!(
            f.check.as_str(),
            "heavy-query" | "unused-index" | "duplicate-index" | "memory-grant" | "network-wait" | "unstable-query"
        ) && !measures_no_time(f)
    }) {
        let Some(share) = f.impact.db_time_share else {
            continue;
        };
        let weight =
            if matches!(f.check.as_str(), "plan-warnings" | "plan-insights") { f.severity } else { Severity::High };
        let parts: Vec<_> = match f.queries.as_slice() {
            [one] => vec![(one.id.clone(), share)],
            refs => {
                let weights: Option<Vec<_>> = refs.iter().map(|r| Some((r.id.clone(), own_share(&r.id)?))).collect();
                let Some(weights) = weights else {
                    continue;
                };
                let total: f64 = weights.iter().map(|(_, w)| w).sum();
                if total <= 0.0 {
                    continue;
                }
                weights.into_iter().map(|(id, w)| (id, share * w / total)).collect()
            }
        };
        for (id, part) in parts {
            causes.entry(id).or_default().push((at, weight, part));
        }
    }
    causes
}

#[cfg(test)]
pub(super) fn explained_heavy_query(s: &Snapshot, findings: &mut Vec<Finding>) {
    Rule(Policy::Heavy).refine(s, findings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Category, Fix, QueryRef};

    fn snapshot() -> Snapshot {
        let mut s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let w = s.workload.as_mut().unwrap();
        w.timeline = None;
        w.window_hours = 24.0;
        w.queries.truncate(1);
        let q = &mut w.queries[0];
        q.executions = 100.0;
        q.mean_time_ms = 10_000.0;
        q.total_time_ms = 1_000_000.0;
        q.first_seen = None;
        q.last_seen = None;
        q.trend = None;
        q.costs = None;
        w.total_time_ms = q.total_time_ms;
        s
    }
    fn finding(check: &str, key: &str, query: &str) -> Finding {
        let mut f = support::finding(check, key, Category::Queries, support::subject(SubjectKind::Query, key));
        f.queries = vec![QueryRef { id: query.into(), plan_node: None, note: None }];
        f.impact.db_time_share = Some(1.0);
        f.severity = Severity::High;
        f
    }

    fn spilling_sort_snapshot() -> Snapshot {
        let mut snapshot = snapshot();
        let plan = snapshot.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap();
        plan.kind = PlanKind::Actual;
        let mut hash = plan.root.clone();
        hash.id = 3;
        hash.kind = NodeKind::Join;
        hash.op = "Hash Join".into();
        hash.children.clear();
        hash.warnings = vec![PlanWarning::Spill];
        plan.root.id = 8;
        plan.root.kind = NodeKind::Sort;
        plan.root.op = "Sort".into();
        plan.root.warnings = vec![PlanWarning::Spill];
        plan.root.children = vec![hash];
        snapshot
    }

    fn sort_and_spill(snapshot: &Snapshot, sort_node: Option<u32>, spill_node: Option<u32>) -> Vec<Finding> {
        let id = &snapshot.queries()[0].id;
        let mut sort = finding("plan-insights", "native-sort", id);
        sort.facts.cause = Some(FindingCause::Sort);
        sort.queries[0].plan_node = sort_node;
        let mut spill = finding("plan-insights", "native-spill", id);
        spill.facts.cause = Some(FindingCause::Spill);
        spill.queries[0].plan_node = spill_node;
        vec![sort, spill]
    }
    fn index_pair(s: &Snapshot) -> Vec<Finding> {
        let mut candidate = finding("filtered-scan", "shop.orders(state)", &s.queries()[0].id);
        candidate.subject = support::subject(SubjectKind::Table, "shop.orders");
        candidate.facts.index_keys = vec!["state".into()];
        candidate.fix = Some(Fix {
            summary: "Candidate index".into(),
            sql: Some("CREATE INDEX ix_orders_state ON shop.orders (state)".into()),
            cost: None,
        });
        let mut fk = finding("unindexed-fk", "shop.orders(state)", &s.queries()[0].id);
        fk.subject = candidate.subject.clone();
        fk.facts.index_keys = candidate.facts.index_keys.clone();
        vec![candidate, fk]
    }

    #[test]
    fn a_covered_finding_names_the_cause_that_remains() {
        let s = snapshot();
        let id = &s.queries()[0].id;
        let mut findings = vec![finding("heavy-query", id, id), finding("regression", id, id)];
        let trace = Rule(Policy::Heavy).refine_traced(&s, &mut findings);
        assert_eq!(findings.len(), 1);
        assert!(matches!(&trace[0].outcome,RefinementOutcome::Covered {by} if by==&vec![findings[0].id.clone()]));
        assert_eq!(trace[0].finding, Some(format!("heavy-query:{id}")));
    }

    #[test]
    fn unknown_time_or_missing_allocating_queries_do_not_explain_time_away() {
        let s = snapshot();
        let id = &s.queries()[0].id;
        let mut heavy = finding("heavy-query", id, id);
        let cause = finding("regression", id, id);
        heavy.impact.db_time_share = None;
        assert!(decide(Policy::Heavy, &s, &[heavy.clone(), cause.clone()]).is_empty());
        heavy.impact.db_time_share = Some(1.0);
        let mut mixed = cause;
        mixed.queries.push(QueryRef { id: "not-in-the-scan".into(), plan_node: None, note: None });
        assert!(decide(Policy::Heavy, &s, &[heavy, mixed]).is_empty());
    }

    #[test]
    fn a_sort_with_no_statement_evidence_is_not_vacuously_covered() {
        let s = snapshot();
        let id = &s.queries()[0].id;
        let mut sort = finding("plan-insights", "big-sort:shop.orders", id);
        sort.facts.cause = Some(FindingCause::Sort);
        sort.queries.clear();
        assert!(decide(Policy::SpillSort, &s, &[sort, finding("temp-spill", id, id)]).is_empty());
    }

    #[test]
    fn recorded_temporary_work_does_not_cover_an_independent_sort_or_explain_query_time_away() {
        let s = spilling_sort_snapshot();
        let id = &s.queries()[0].id;
        let mut sort = finding("plan-insights", "sort", id);
        sort.facts.cause = Some(FindingCause::Sort);
        sort.queries[0].plan_node = Some(8);
        let heavy = finding("heavy-query", id, id);
        let mut observed = finding("temp-spill", id, id);
        observed.actionability = culprit_core::Actionability::Information;
        observed.impact.db_time_share = None;
        observed.impact.time_ms_per_day = None;
        assert!(decide(Policy::SpillSort, &s, &[sort.clone(), observed.clone()]).is_empty());
        assert!(decide(Policy::Heavy, &s, &[heavy, observed]).is_empty());

        let mut confirmed = finding("plan-warnings", "native-spill", id);
        confirmed.facts.cause = Some(FindingCause::Spill);
        confirmed.queries[0].plan_node = Some(8);
        assert_eq!(decide(Policy::SpillSort, &s, &[sort, confirmed]).len(), 1);
    }

    #[test]
    fn only_the_same_native_spilling_sort_in_the_selected_actual_plan_covers_its_sort_finding() {
        let s = spilling_sort_snapshot();
        let mut findings = sort_and_spill(&s, Some(8), Some(8));
        let trace = Rule(Policy::SpillSort).refine_traced(&s, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].facts.cause, Some(FindingCause::Spill));
        assert_eq!(trace.len(), 1);
        assert!(matches!(&trace[0].outcome, RefinementOutcome::Covered { by } if by == &vec![findings[0].id.clone()]));
    }

    #[test]
    fn a_native_spill_warning_keeps_its_typed_operator_but_does_not_cover_independently_measured_query_time() {
        let s = spilling_sort_snapshot();
        let id = &s.queries()[0].id;
        let native = culprit_core::Check::run(&crate::queries::PlanWarningsCheck, &s);
        let spill = native
            .into_iter()
            .find(|finding| finding.facts.cause == Some(FindingCause::Spill))
            .expect("native spill warning");
        assert_eq!(spill.queries[0].id, *id);
        assert_eq!(spill.queries[0].plan_node, Some(8));
        assert_eq!(spill.impact.db_time_share, Some(1.0), "query context is distinct from a timed spill contribution");
        let heavy = finding("heavy-query", id, id);
        assert!(decide(Policy::Heavy, &s, &[heavy, spill]).is_empty());
    }

    #[test]
    fn a_hash_spill_or_an_unknown_operator_cannot_cover_an_independent_sort_in_the_same_statement() {
        let mut s = spilling_sort_snapshot();
        s.workload.as_mut().unwrap().queries[0].plan.as_mut().unwrap().root.warnings.clear();
        assert!(decide(Policy::SpillSort, &s, &sort_and_spill(&s, Some(8), Some(3))).is_empty());
        let s = spilling_sort_snapshot();
        for (sort, spill) in [(None, Some(8)), (Some(8), None), (None, None), (Some(99), Some(99)), (Some(8), Some(3))]
        {
            assert!(decide(Policy::SpillSort, &s, &sort_and_spill(&s, sort, spill)).is_empty());
        }
        let mut grouped = sort_and_spill(&s, Some(8), Some(8));
        grouped[0].queries.push(QueryRef { id: "unqualified-statement".into(), plan_node: Some(8), note: None });
        assert!(decide(Policy::SpillSort, &s, &grouped).is_empty());
    }

    #[test]
    fn absent_estimated_or_ambiguous_selected_plan_nodes_do_not_establish_operator_coverage() {
        for shape in ["estimated", "generic", "missing-plan", "wrong-kind", "no-verdict", "duplicate-node"] {
            let mut s = spilling_sort_snapshot();
            let query = &mut s.workload.as_mut().unwrap().queries[0];
            if shape == "missing-plan" {
                query.plan = None;
            } else {
                let plan = query.plan.as_mut().unwrap();
                match shape {
                    "estimated" => plan.kind = PlanKind::Estimated,
                    "generic" => plan.kind = PlanKind::Generic,
                    "wrong-kind" => plan.root.kind = NodeKind::Join,
                    "no-verdict" => plan.root.warnings.clear(),
                    "duplicate-node" => plan.root.children[0].id = 8,
                    _ => unreachable!(),
                }
            }
            assert!(decide(Policy::SpillSort, &s, &sort_and_spill(&s, Some(8), Some(8))).is_empty(), "{shape}");
        }
    }

    #[test]
    fn a_candidate_withheld_for_missing_work_cannot_hide_the_foreign_key() {
        let mut s = snapshot();
        let w = s.workload.as_mut().unwrap();
        let q = &mut w.queries[0];
        q.mean_time_ms = 0.001;
        q.total_time_ms = 0.1;
        w.total_time_ms = 0.1;
        assert!(!lifecycle::has_measured_index_work(&s, &index_pair(&s)[0]));
        for reverse in [false, true] {
            let mut findings = index_pair(&s);
            if reverse {
                lifecycle::Rule(lifecycle::Policy::IndexWork).refine(&s, &mut findings);
            }
            Rule(Policy::ForeignKey).refine(&s, &mut findings);
            if !reverse {
                lifecycle::Rule(lifecycle::Policy::IndexWork).refine(&s, &mut findings);
            }
            assert!(findings.iter().any(|f| f.check == "unindexed-fk"));
        }
        let material = snapshot();
        let mut findings = index_pair(&material);
        let trace = Rule(Policy::ForeignKey).refine_traced(&material, &mut findings);
        assert_eq!(findings.len(), 1);
        assert!(matches!(&trace[0].outcome,RefinementOutcome::Covered {by} if by==&vec![findings[0].id.clone()]));
    }

    #[test]
    fn filtered_or_sql_less_candidates_leave_foreign_key_advice_visible() {
        let s = snapshot();
        for sql in [None, Some("CREATE INDEX ix_orders_state ON shop.orders (state) WHERE state = 1".into())] {
            let mut findings = index_pair(&s);
            findings[0].fix.as_mut().unwrap().sql = sql;
            Rule(Policy::ForeignKey).refine(&s, &mut findings);
            assert_eq!(findings.len(), 2);
        }
    }

    #[test]
    fn translating_human_text_does_not_change_coverage() {
        let s = spilling_sort_snapshot();
        let id = &s.queries()[0].id;
        let mut rewrite = finding("plan-insights", "non-sargable:shop.orders", id);
        rewrite.facts.cause = Some(FindingCause::PredicateRewrite);
        rewrite.subject = support::subject(SubjectKind::Table, "shop.orders");
        let mut sort = finding("plan-insights", "big-sort:shop.orders", id);
        sort.facts.cause = Some(FindingCause::Sort);
        sort.queries[0].plan_node = Some(8);
        let mut native_spill = finding("plan-warnings", "native-spill", id);
        native_spill.facts.cause = Some(FindingCause::Spill);
        native_spill.queries[0].plan_node = Some(8);
        native_spill.severity = Severity::High;
        let pairs = [
            (Policy::Rewrite, vec![finding("filtered-scan", "shop.orders", id), rewrite]),
            (Policy::Heavy, vec![finding("heavy-query", id, id), finding("regression", id, id)]),
            (Policy::ForeignKey, index_pair(&s)),
            (Policy::Regression, vec![finding("plan-regression", id, id), finding("regression", id, id)]),
            (Policy::SpillSort, vec![sort, native_spill]),
        ];
        for (policy, mut findings) in pairs {
            let expected = decide(policy, &s, &findings);
            assert!(!expected.is_empty());
            for f in &mut findings {
                f.id = "opaque:spill:force:unrelated".into();
                f.title = "Other title".into();
                f.summary = "Different wording".into();
                f.caveats = vec!["Other caveat".into()];
                f.verify = vec!["Other words".into()];
                for metric in &mut f.metrics {
                    metric.label = "Translated evidence label".into();
                    if metric.text.is_some() {
                        metric.text = Some("Translated evidence text".into());
                    }
                }
                if let Some(fix) = &mut f.fix {
                    fix.summary = "Other fix wording".into();
                }
            }
            assert_eq!(expected, decide(policy, &s, &findings));
        }
    }
}
