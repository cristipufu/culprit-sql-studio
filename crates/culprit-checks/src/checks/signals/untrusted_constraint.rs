//! SQL Server untrusted/disabled constraints and PostgreSQL/YSQL deferred validation. Native YSQL findings describe
//! unvalidated data without assuming PostgreSQL planner or lock behavior.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn untrusted_constraint_info() -> CheckInfo {
    CheckInfo {
        id: "untrusted-constraint".into(),
        title: t!("check.untrusted_constraint.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte],
        requires: vec![],
        description: t!("check.untrusted_constraint.info.description"),
    }
}

check!(
    UntrustedConstraintCheck: untrusted_constraint_info,
    decide = untrusted_tables,
    render = untrusted_constraint_finding,
    case<'s> = UntrustedTable<'s>,
    evidence = &[Evidence::Source(culprit_core::source_id::CONSTRAINTS)],
);

/// Whether PostgreSQL's planner reads these CHECK constraints at all: constraint exclusion looks at inheritance
/// children only (constraint_exclusion = partition, the default), or at every table with constraint_exclusion = on.
pub(super) fn planner_reads_checks(snapshot: &Snapshot, list: &[&ConstraintIssue]) -> bool {
    list.iter().any(|c| c.inheritance_child)
        || snapshot.setting("constraint_exclusion").is_some_and(|s| crate::config::is_on(&s.value))
}

/// A table's constraints the engine doesn't vouch for the existing rows of.
pub struct UntrustedTable<'s> {
    schema: String,
    table: String,
    list: Vec<&'s ConstraintIssue>,
    /// How many are switched off altogether (SQL Server).
    disabled: usize,
    engine: Unvalidated,
}

/// What an unvalidated constraint means on this engine.
#[derive(Clone, Copy, PartialEq)]
pub enum Unvalidated {
    /// SQL Server doesn't trust it: its optimizer skips nothing on its account.
    Untrusted,
    /// YugabyteDB checks new rows but not the old ones; nothing is said of its planner.
    Deferred,
    /// PostgreSQL's planner reads CHECK constraints here (constraint exclusion): on an inheritance child, or everywhere
    /// with constraint_exclusion = on.
    PlannerReads { inheritance_child: bool },
    /// PostgreSQL's planner doesn't read them here: it is about the data, and often deliberate.
    DataOnly,
}

fn untrusted_tables(snapshot: &Snapshot) -> Vec<UntrustedTable<'_>> {
    let mut by_table: BTreeMap<(String, String), Vec<&ConstraintIssue>> = BTreeMap::new();
    for c in &snapshot.constraints {
        by_table.entry((c.schema.clone(), c.table.clone())).or_default().push(c);
    }
    by_table
        .into_iter()
        .map(|((schema, table), list)| {
            let engine = match snapshot.server.engine {
                Engine::Mssql => Unvalidated::Untrusted,
                Engine::Yugabyte => Unvalidated::Deferred,
                _ if planner_reads_checks(snapshot, &list) => {
                    Unvalidated::PlannerReads { inheritance_child: list.iter().any(|c| c.inheritance_child) }
                }
                _ => Unvalidated::DataOnly,
            };
            let disabled = list.iter().filter(|c| c.disabled).count();
            UntrustedTable { schema, table, list, disabled, engine }
        })
        .collect()
}

fn untrusted_constraint_finding(snapshot: &Snapshot, u: UntrustedTable<'_>) -> Finding {
    let UntrustedTable { schema, table, list, disabled, engine } = u;
    let name = format!("{schema}.{table}");
    let mut f = finding("untrusted-constraint", &name, Category::Maintenance, subject(SubjectKind::Table, &name));
    let kind = |c: &ConstraintIssue| match c.kind {
        ConstraintKind::ForeignKey => "foreign_key",
        ConstraintKind::Check => "check",
    };
    let count = list.len().to_string();
    let mssql = engine == Unvalidated::Untrusted;
    let ysql = engine == Unvalidated::Deferred;
    f.title = match (list.as_slice(), mssql) {
        ([one], _) if one.disabled => {
            t!("check.untrusted_constraint.title.switched_off", kind = kind(one), name = &one.name, table = &table)
        }
        ([one], true) => {
            t!("check.untrusted_constraint.title.untrusted", kind = kind(one), name = &one.name, table = &table)
        }
        ([one], false) if ysql => {
            t!("check.untrusted_constraint.title.deferred", kind = kind(one), name = &one.name, table = &table)
        }
        ([one], false) => t!("check.untrusted_constraint.title.not_valid", name = &one.name, table = &table),
        (_, true) => t!("check.untrusted_constraint.title.untrusted_many", count = &count, table = &table),
        (_, false) if ysql => t!("check.untrusted_constraint.title.deferred_many", count = &count, table = &table),
        (_, false) => t!("check.untrusted_constraint.title.not_valid_many", count = &count, table = &table),
    };
    let names = format::list(&list.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    let n = list.len();
    f.summary = match engine {
        Unvalidated::Untrusted => {
            let mut summary = vec![t!("check.untrusted_constraint.summary.nocheck", names = &names, n = n)];
            if disabled > 0 {
                summary.push(t!("check.untrusted_constraint.summary.switched_off"));
            }
            summary.join(" ")
        }
        Unvalidated::Deferred => t!("check.untrusted_constraint.summary.deferred", names = &names, n = n),
        Unvalidated::PlannerReads { inheritance_child: true } => {
            t!("check.untrusted_constraint.summary.planner_child", names = &names, n = n)
        }
        Unvalidated::PlannerReads { inheritance_child: false } => {
            t!("check.untrusted_constraint.summary.planner_on", names = &names, n = n)
        }
        // The planner reads CHECK constraints only on inheritance children (constraint_exclusion = partition, the
        // default): here a NOT VALID one is about the data, and often deliberate.
        Unvalidated::DataOnly => t!("check.untrusted_constraint.summary.data_only", names = &names, n = n),
    };
    f.severity = if disabled > 0 { Severity::Medium } else { Severity::Low };
    severity_score(&mut f);
    f.impact.label = if ysql {
        t!("check.untrusted_constraint.impact.not_validated", count = &count)
    } else {
        t!("check.untrusted_constraint.impact.untrusted", count = &count)
    };
    f.confidence = Confidence::High;
    f.metrics = vec![Metric::new(t!("check.untrusted_constraint.metric.constraints"), n as f64, Unit::Count)];
    let sql_engine = snapshot.server.engine;
    let target = qualified(sql_engine, &schema, &table);
    f.fix = Some(if mssql {
        Fix {
            summary: t!("check.untrusted_constraint.fix.check", n = n),
            sql: Some(
                list.iter()
                    .map(|c| {
                        format!("ALTER TABLE {target} WITH CHECK CHECK CONSTRAINT {};", quote(sql_engine, &c.name))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            cost: Some(t!("check.untrusted_constraint.fix.cost_reads")),
        }
    } else {
        Fix {
            summary: if ysql {
                t!("check.untrusted_constraint.fix.validate_ysql")
            } else {
                t!("check.untrusted_constraint.fix.validate")
            },
            sql: Some(
                list.iter()
                    .map(|c| format!("ALTER TABLE {target} VALIDATE CONSTRAINT {};", quote(sql_engine, &c.name)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            cost: Some(if ysql {
                t!("check.untrusted_constraint.fix.cost_ysql")
            } else {
                t!("check.untrusted_constraint.fix.cost_reads")
            }),
        }
    });
    f.verify = vec![t!("check.untrusted_constraint.verify")];
    f
}
