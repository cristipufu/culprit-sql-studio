//! Resolve shared maintenance causes from typed source facts, then apply state and render their explanation.
use super::*;
use culprit_core::{CleanupHolder, RefinementDecision, RefinementOutcome};

#[derive(Clone, Copy)]
pub(super) enum Policy {
    Compiles,
    Statistics,
    Slot,
}
impl Policy {
    fn id(self) -> &'static str {
        match self {
            Self::Compiles => "plan-cache-carries-compiles",
            Self::Statistics => "dead-rows-carry-statistics",
            Self::Slot => "slot-carries-cleanup",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Compiles => "the plan cache finding carries the compile load",
            Self::Statistics => "dead rows absorb the table's stale statistics",
            Self::Slot => "a replication slot's finding carries the cleanup it holds back",
        }
    }
}
pub(super) struct Rule(pub Policy);

/// The compile pressure metrics the plan cache finding carries: its compiles, not its recompiles.
const COMPILES: [&str; 2] =
    [key!("check.compile_pressure.metric.compiles"), key!("check.compile_pressure.metric.per_minute")];

#[derive(Clone, Debug, PartialEq)]
enum Evidence {
    Compiles,
    Statistics { changes: Option<f64> },
    Slot { transactions: f64, catalog_only: bool },
}
#[derive(Clone, Debug, PartialEq)]
struct Decision {
    from: usize,
    into: usize,
    evidence: Evidence,
}

fn decide(policy: Policy, s: &Snapshot, findings: &[Finding]) -> Vec<Decision> {
    let mut out = Vec::new();
    match policy {
        Policy::Compiles => {
            if let (Some(from), Some(into)) = (
                findings.iter().position(|f| f.check == "compile-pressure"),
                findings.iter().position(|f| f.id == "query-family:plan-cache"),
            ) {
                out.push(Decision { from, into, evidence: Evidence::Compiles });
            }
        }
        Policy::Statistics => {
            for (from, f) in findings.iter().enumerate().filter(|(_, f)| {
                f.check == "stale-stats" && f.id.eq_ignore_ascii_case(&format!("stale-stats:{}", f.subject.name))
            }) {
                if let Some(into) = findings.iter().position(|g| {
                    g.check == "dead-rows"
                        && g.id.eq_ignore_ascii_case(&format!("dead-rows:{}", f.subject.name))
                        && g.subject.name.eq_ignore_ascii_case(&f.subject.name)
                }) {
                    let changes = f
                        .metrics
                        .iter()
                        .find(|m| m.is(crate::checks::maintenance::CHANGES_SINCE_UPDATE))
                        .map(|m| m.value);
                    out.push(Decision { from, into, evidence: Evidence::Statistics { changes } });
                }
            }
        }
        Policy::Slot => {
            let Some(holder) = s
                .pressure
                .as_ref()
                .and_then(|p| p.oldest_transaction.as_ref())
                .filter(|h| h.holder == CleanupHolder::ReplicationSlot)
            else {
                return out;
            };
            let Some(transactions) = holder.xid_age.filter(|n| n.is_finite() && *n > 0.0) else {
                return out;
            };
            if let (Some(from), Some(into)) = (
                findings.iter().position(|f| f.id == format!("held-cleanup:{}", holder.session)),
                findings.iter().position(|f| f.id == format!("replica-lag:{}", holder.session)),
            ) {
                out.push(Decision {
                    from,
                    into,
                    evidence: Evidence::Slot { transactions, catalog_only: holder.catalog_only },
                });
            }
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
        let sources: Vec<_> = decisions.iter().map(|d| findings[d.from].clone()).collect();
        let trace = decisions
            .iter()
            .map(|d| RefinementDecision {
                rule: self.0.id().into(),
                finding: Some(findings[d.from].id.clone()),
                outcome: RefinementOutcome::Absorbed { into: findings[d.into].id.clone() },
                queries: findings[d.from].queries.iter().map(|r| r.id.clone()).collect(),
                tables: if findings[d.from].subject.kind == SubjectKind::Table {
                    vec![findings[d.from].subject.name.clone()]
                } else {
                    Vec::new()
                },
            })
            .collect();
        for (d, source) in decisions.iter().zip(&sources) {
            let f = &mut findings[d.into];
            f.severity = f.severity.max(source.severity);
            f.impact.score = f.impact.score.max(source.impact.score);
            match d.evidence {
                Evidence::Compiles => {
                    f.metrics.extend(source.metrics.iter().filter(|m| COMPILES.iter().any(|k| m.is(k))).cloned())
                }
                Evidence::Slot { .. } => f
                    .metrics
                    .extend(source.metrics.iter().filter(|m| m.is(key!("check.held_cleanup.metric.behind"))).cloned()),
                Evidence::Statistics { .. } => {
                    if let Some(fix) = f.fix.as_mut()
                        && !fix.sql.as_deref().is_some_and(|s| s.contains("ANALYZE"))
                    {
                        fix.sql = fix.sql.take().map(|s| format!("{s}\nANALYZE {};", source.subject.name));
                    }
                }
            }
        }
        for (d, source) in decisions.iter().zip(&sources) {
            let f = &mut findings[d.into];
            f.summary.push(' ');
            match d.evidence {
                Evidence::Compiles => {
                    f.summary.push_str(&t!("refine.maintenance.compiles", title = source.title.trim_end_matches('.')));
                    f.verify.extend(source.verify.iter().cloned());
                }
                Evidence::Statistics { changes } => f.summary.push_str(&match changes {
                    Some(c) => t!("refine.maintenance.statistics_changes", changes = format::count(c)),
                    None => t!("refine.maintenance.statistics"),
                }),
                Evidence::Slot { transactions, catalog_only } => {
                    let transactions = format::count(transactions);
                    f.summary.push_str(&if catalog_only {
                        t!("refine.maintenance.slot_catalog", transactions = transactions)
                    } else {
                        t!("refine.maintenance.slot_vacuum", transactions = transactions)
                    });
                }
            }
        }
        let gone: BTreeSet<_> = decisions.iter().map(|d| d.from).collect();
        let mut at = 0;
        findings.retain(|_| {
            let keep = !gone.contains(&at);
            at += 1;
            keep
        });
        trace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Category, OpenTransaction};
    fn slot() -> (Snapshot, Vec<Finding>) {
        let mut s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        s.pressure.get_or_insert_with(Default::default).oldest_transaction = Some(OpenTransaction {
            holder: CleanupHolder::ReplicationSlot,
            session: "consumer_a".into(),
            xid_age: Some(2_000_000.0),
            ..Default::default()
        });
        let finding = |check: &str| {
            support::finding(
                check,
                "consumer_a",
                Category::Maintenance,
                support::subject(SubjectKind::Database, "shop"),
            )
        };
        (s, vec![finding("held-cleanup"), finding("replica-lag")])
    }
    #[test]
    fn cleanup_relationship_uses_the_holder_instead_of_the_title() {
        let (s, mut findings) = slot();
        findings[0].title = "A translated observation without the original prefix".into();
        let decisions = decide(Policy::Slot, &s, &findings);
        assert_eq!(decisions.len(), 1);
        let trace = Rule(Policy::Slot).refine_traced(&s, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "replica-lag:consumer_a");
        assert!(findings[0].summary.contains("holds VACUUM back by 2.0M transactions"));
        assert!(matches!(&trace[0].outcome,RefinementOutcome::Absorbed {into} if into==&findings[0].id));
    }
    #[test]
    fn a_title_alone_cannot_turn_a_session_into_a_replication_slot() {
        let (mut s, mut findings) = slot();
        s.pressure.as_mut().unwrap().oldest_transaction.as_mut().unwrap().holder = CleanupHolder::Session;
        findings[0].title = "Replication slot consumer_a holds VACUUM back".into();
        assert!(decide(Policy::Slot, &s, &findings).is_empty());
        Rule(Policy::Slot).refine(&s, &mut findings);
        assert_eq!(findings.len(), 2);
    }
    #[test]
    fn catalog_only_horizons_do_not_claim_to_hold_back_every_application_table() {
        let (mut s, mut findings) = slot();
        s.pressure.as_mut().unwrap().oldest_transaction.as_mut().unwrap().catalog_only = true;
        Rule(Policy::Slot).refine(&s, &mut findings);
        assert!(findings[0].summary.contains("system-catalog cleanup"));
        assert!(!findings[0].summary.contains("any table, in any database"));
        assert!(findings[0].summary.contains("does not establish"));
    }

    // The metrics a rule carries or reads are found by what they are, in whatever language they were said.
    #[test]
    fn carried_metrics_are_found_by_key_in_another_language() {
        use culprit_core::{Metric, Unit};
        culprit_core::i18n::scope("fr", || {
            let (s, mut findings) = slot();
            findings[0].metrics = vec![
                Metric::said(key!("check.held_cleanup.metric.open"), 60_000.0, Unit::Ms),
                Metric::said(key!("check.held_cleanup.metric.behind"), 2_000_000.0, Unit::Count),
            ];
            assert_ne!(findings[0].metrics[1].label, "Transactions behind");
            Rule(Policy::Slot).refine(&s, &mut findings);
            let carried: Vec<_> = findings[0].metrics.iter().map(|m| m.key.as_str()).collect();
            assert_eq!(carried, [key!("check.held_cleanup.metric.behind")]);

            let compiles = |key: &str| Metric::said(key, 40.0, Unit::Count);
            let mut pressure = support::finding(
                "compile-pressure",
                "server",
                Category::Configuration,
                support::subject(SubjectKind::Database, "shop"),
            );
            pressure.metrics = vec![
                compiles(key!("check.compile_pressure.metric.compiles")),
                compiles(key!("check.compile_pressure.metric.per_minute")),
                compiles(key!("check.compile_pressure.metric.recompiles")),
            ];
            let cache = support::finding(
                "query-family",
                "plan-cache",
                Category::Queries,
                support::subject(SubjectKind::Database, "shop"),
            );
            let mut findings = vec![pressure, cache];
            Rule(Policy::Compiles).refine(&s, &mut findings);
            let carried: Vec<_> = findings[0].metrics.iter().map(|m| m.key.as_str()).collect();
            assert_eq!(carried, COMPILES);

            let table = support::subject(SubjectKind::Table, "public.orders");
            let mut stats = support::finding("stale-stats", "public.orders", Category::Maintenance, table.clone());
            stats.metrics = vec![Metric::said(crate::checks::maintenance::CHANGES_SINCE_UPDATE, 1234.0, Unit::Count)];
            let dead = support::finding("dead-rows", "public.orders", Category::Maintenance, table);
            let decided = decide(Policy::Statistics, &s, &[stats, dead]);
            assert_eq!(decided[0].evidence, Evidence::Statistics { changes: Some(1234.0) });
        });
    }

    #[test]
    fn dead_rows_only_absorbs_base_stale_stats_and_not_correlated_or_scale_factor_subfindings() {
        let s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        let table = support::subject(SubjectKind::Table, "public.orders");
        let correlated = support::finding(
            "stale-stats",
            "correlated:public.orders(status,region)",
            Category::Maintenance,
            table.clone(),
        );
        let scale_factor =
            support::finding("dead-rows", "scale-factor:public.orders", Category::Maintenance, table.clone());
        let base_stats = support::finding("stale-stats", "public.orders", Category::Maintenance, table.clone());
        let base_dead = support::finding("dead-rows", "public.orders", Category::Maintenance, table);

        // Sub-findings (correlated stats, scale-factor tuning) are never absorbed into or by each other.
        assert!(decide(Policy::Statistics, &s, &[correlated.clone(), base_dead.clone()]).is_empty());
        assert!(decide(Policy::Statistics, &s, &[base_stats.clone(), scale_factor]).is_empty());

        // Base stale-stats is absorbed into base dead-rows while correlated stats stays independent.
        let mut findings = vec![correlated, base_stats, base_dead];
        Rule(Policy::Statistics).refine(&s, &mut findings);
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|f| f.id == "stale-stats:correlated:public.orders(status,region)"));
        assert!(findings.iter().any(|f| f.id == "dead-rows:public.orders"));
    }

    #[test]
    fn disabled_autovacuum_and_scale_factor_deduplicate_with_base_and_config_findings() {
        let mut s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        // 1. A table with autovacuum_enabled=false and >= 20% dead rows emits one High `dead-rows:<table>` finding
        // (whose fix already sets `autovacuum_enabled = true`), not a duplicate `dead-rows:autovacuum-disabled:<table>`.
        let orders = s.tables.iter_mut().find(|t| t.name == "orders").unwrap();
        orders.rows = Some(100_000.0);
        orders.dead_rows = Some(50_000.0);
        orders.storage =
            Some(culprit_core::TableStorage { options: vec!["autovacuum_enabled=false".into()], ..Default::default() });
        let findings = crate::checks::maintenance::dead_rows(&s);
        let base = findings.iter().find(|f| f.id == "dead-rows:public.orders").expect("base dead-rows finding");
        assert_eq!(base.severity, Severity::High);
        assert!(!findings.iter().any(|f| f.id == "dead-rows:autovacuum-disabled:public.orders"));

        // 2. A >= 10M-row table inheriting the server's autovacuum_vacuum_scale_factor = 0.2 with >= 1% churn is
        // covered by `config:autovacuum-scale`, so `dead-rows:scale-factor:<table>` does not duplicate it.
        let events = s.tables.iter_mut().find(|t| t.name == "events").unwrap();
        events.rows = Some(15_000_000.0);
        events.dead_rows = Some(200_000.0);
        events.updates = Some(500_000.0);
        let report = crate::analyze(s);
        assert!(report.findings.iter().any(|f| f.id == "config:autovacuum-scale"));
        assert!(!report.findings.iter().any(|f| f.id == "dead-rows:scale-factor:public.events"));
    }
}
