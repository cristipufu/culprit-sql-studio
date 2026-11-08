//! Wording is a final presentation operation with access only to display fields, never policy state or executable SQL.
use super::*;
use culprit_core::{PresentationAspect, RefinementDecision, RefinementOutcome};

#[derive(Clone, Copy)]
pub(super) enum Policy {
    AbsoluteCost,
    Names,
    Caveats,
}
impl Policy {
    fn id(self) -> &'static str {
        match self {
            Self::AbsoluteCost => "absolute-query-cost",
            Self::Names => "database-naming",
            Self::Caveats => "repeated-caveats",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::AbsoluteCost => "query evidence leads with absolute cost and period",
            Self::Names => "names without the database",
            Self::Caveats => "identical complete caveats are shown once",
        }
    }
}
pub(super) struct Rule(pub Policy);

struct Prose<'a> {
    title: &'a mut String,
    summary: &'a mut String,
    impact_label: &'a mut String,
    caveats: &'a mut Vec<String>,
    verify: &'a mut Vec<String>,
    fix_summary: Option<&'a mut String>,
}
impl<'a> From<&'a mut Finding> for Prose<'a> {
    fn from(f: &'a mut Finding) -> Self {
        Self {
            title: &mut f.title,
            summary: &mut f.summary,
            impact_label: &mut f.impact.label,
            caveats: &mut f.caveats,
            verify: &mut f.verify,
            fix_summary: f.fix.as_mut().map(|fix| &mut fix.summary),
        }
    }
}

enum Change {
    Cost { cost: String, title: Option<String>, direct: bool },
    Activity { runs: String, incomplete: bool },
    Names { database: String },
    Caveats { replacement: Vec<String> },
}
struct Decision {
    at: usize,
    change: Change,
}
impl Change {
    fn aspect(&self) -> PresentationAspect {
        match self {
            Self::Cost { .. } => PresentationAspect::AbsoluteCost,
            Self::Activity { .. } => PresentationAspect::ObservedActivity,
            Self::Names { .. } => PresentationAspect::DatabaseNaming,
            Self::Caveats { .. } => PresentationAspect::RepeatedCaveats,
        }
    }
    fn render(&self, prose: Prose<'_>) {
        match self {
            Self::Cost { cost, title, direct } => {
                if let Some(title) = title {
                    *prose.title = title.clone();
                }
                let lead = if *direct {
                    t!("refine.presentation.cost", cost = cost)
                } else {
                    t!("refine.presentation.cost_of_the_statements", cost = cost)
                };
                *prose.summary = [lead, std::mem::take(prose.summary)].join(" ");
                if *direct {
                    *prose.impact_label = cost.clone();
                }
            }
            Self::Activity { runs, incomplete } => {
                let coverage = if *incomplete {
                    t!("refine.presentation.activity_incomplete")
                } else {
                    t!("refine.presentation.activity_counted")
                };
                prose.summary.push(' ');
                prose.summary.push_str(&[t!("refine.presentation.latest_activity", runs = runs), coverage].join(" "));
            }
            Self::Names { database } => {
                *prose.title = without_database(prose.title, database);
                *prose.summary = without_database(prose.summary, database);
                for line in prose.verify.iter_mut().chain(prose.caveats.iter_mut()) {
                    *line = without_database(line, database);
                }
                if let Some(summary) = prose.fix_summary {
                    *summary = without_database(summary, database);
                }
            }
            Self::Caveats { replacement } => *prose.caveats = replacement.clone(),
        }
    }
}

fn decide(policy: Policy, s: &Snapshot, findings: &[Finding]) -> Vec<Decision> {
    let mut out = Vec::new();
    for (at, f) in findings.iter().enumerate() {
        match policy {
            Policy::AbsoluteCost if !f.inactive && f.impact.db_time_share.is_some() => {
                let Some(mut queries) = s
                    .workload
                    .as_ref()
                    .and_then(|w| f.queries.iter().map(|r| w.query(&r.id)).collect::<Option<Vec<_>>>())
                else {
                    continue;
                };
                queries.sort_by(|a, b| a.id.cmp(&b.id));
                queries.dedup_by(|a, b| a.id == b.id);
                let same_share = support::query_impact(s, &queries)
                    .zip(f.impact.db_time_share)
                    .is_some_and(|(impact, share)| (impact.share - share).abs() < 1e-9);
                if same_share && let Some(cost) = support::query_cost_context(s, &queries) {
                    // Heavy-query's title says the cost in place of its share.
                    let title = f
                        .impact
                        .db_time_share
                        .filter(|_| f.check == "heavy-query")
                        .zip(queries.iter().find(|q| q.id == f.subject.name))
                        .and_then(|(share, q)| crate::queries::title_with_cost(s, q, share, &cost));
                    let direct =
                        matches!(f.check.as_str(), "heavy-query" | "chatty-query" | "missing-index" | "filtered-scan");
                    out.push(Decision { at, change: Change::Cost { cost, title, direct } });
                }
                if f.subject.kind == SubjectKind::Query
                    && let [q] = queries.as_slice()
                    && let Some(activity) = s.workload.as_ref().and_then(|w| w.observed_activity(q))
                {
                    out.push(Decision {
                        at,
                        change: Change::Activity { runs: activity.runs_phrase(), incomplete: activity.incomplete },
                    });
                }
            }
            Policy::Names if s.server.engine == Engine::Mssql && !s.server.database.is_empty() => {
                let prefix = format!("[{}].[", s.server.database.replace(']', "]]"));
                let texts = std::iter::once(&f.title)
                    .chain(std::iter::once(&f.summary))
                    .chain(&f.verify)
                    .chain(&f.caveats)
                    .chain(f.fix.iter().map(|fix| &fix.summary));
                if texts.into_iter().any(|text| text.contains(&prefix)) {
                    out.push(Decision { at, change: Change::Names { database: s.server.database.clone() } });
                }
            }
            Policy::Caveats if !f.caveats.is_empty() => {
                // Presentation may discard an identical complete message, never infer or cut sentences from prose.
                let mut said: BTreeSet<&str> =
                    std::iter::once(f.summary.as_str()).chain(f.fix.iter().map(|fix| fix.summary.as_str())).collect();
                let replacement: Vec<_> =
                    f.caveats.iter().filter(|caveat| said.insert(caveat.as_str())).cloned().collect();
                if replacement != f.caveats {
                    out.push(Decision { at, change: Change::Caveats { replacement } });
                }
            }
            _ => {}
        }
    }
    out
}

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
                outcome: RefinementOutcome::Presentation { aspect: d.change.aspect() },
                queries: findings[d.at].queries.iter().map(|r| r.id.clone()).collect(),
                tables: Vec::new(),
            })
            .collect();
        for d in decisions {
            d.change.render(Prose::from(&mut findings[d.at]));
        }
        trace
    }
}

#[cfg(test)]
pub(super) fn caveats_repeat_nothing(s: &Snapshot, findings: &mut Vec<Finding>) {
    Rule(Policy::Caveats).refine(s, findings);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy_fields(f: &Finding) -> serde_json::Value {
        let mut value = serde_json::to_value(f).unwrap();
        for key in ["title", "summary", "caveats", "verify"] {
            value.as_object_mut().unwrap().remove(key);
        }
        value["impact"].as_object_mut().unwrap().remove("label");
        if let Some(fix) = value["fix"].as_object_mut() {
            fix.remove("summary");
        }
        value
    }
    #[test]
    fn presentation_changes_neither_analysis_state_nor_fix_sql() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let mut f = crate::analyze(s.clone()).findings.remove(0);
        let prefix = format!("[{}].[dbo].[orders]", s.server.database);
        f.summary = format!("{prefix} is measured.");
        f.caveats = vec!["A repeated complete message.".into(), "Another caveat.".into()];
        if let Some(fix) = &mut f.fix {
            fix.summary = "A repeated complete message.".into();
            fix.sql = Some(format!("SELECT * FROM {prefix}"));
        }
        let before = policy_fields(&f);
        let mut findings = vec![f];
        for policy in [Policy::AbsoluteCost, Policy::Names, Policy::Caveats] {
            Rule(policy).refine(&s, &mut findings);
            assert_eq!(before, policy_fields(&findings[0]));
        }
        assert!(!findings[0].summary.contains(&format!("[{}]", s.server.database)));
        assert_eq!(findings[0].caveats, ["Another caveat."]);
    }
}
