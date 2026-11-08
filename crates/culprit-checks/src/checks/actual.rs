//! Settings that decide whether Culprit can show what a query actually did: SQL Server's last actual plan per query and
//! live per-operator progress.

use crate::evidence::Requirement as Evidence;
use culprit_core::{Category, CheckInfo, Confidence, Engine, Finding, Fix, Severity, Snapshot, SubjectKind, t};

use culprit_core::Check;

use crate::support::*;

/// The checks this area registers (`crate::all()` runs every area's), in the order their findings tie.
pub(crate) const CHECKS: &[&dyn Check] = &[&LastActualPlansCheck];

pub fn last_actual_plans_info() -> CheckInfo {
    CheckInfo {
        id: "last-actual-plans".into(),
        title: t!("check.last_actual_plans.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql],
        requires: vec![],
        description: t!("check.last_actual_plans.info.description"),
    }
}

check!(
    LastActualPlansCheck: last_actual_plans_info,
    decide = settings_off,
    render = setting_off_finding,
    case<'s> = SettingOff,
    evidence = &[Evidence::Settings, Evidence::Query(crate::evidence::QueryMeasure::Work)],
);

/// A setting that keeps Culprit from showing what a query actually did, off for this database.
pub enum SettingOff {
    /// LAST_QUERY_PLAN_STATS: no last actual plan per cached query. `trace_flag`: the server is one you manage, where
    /// trace flag 2451 turns the same on for the whole server.
    LastQueryPlanStats { trace_flag: bool },
    /// LIGHTWEIGHT_QUERY_PROFILING: no live per-operator progress.
    LightweightQueryProfiling,
}

fn off(snapshot: &Snapshot, name: &str) -> bool {
    snapshot.setting(name).is_some_and(|s| matches!(s.value.trim(), "0" | "OFF" | "off" | "false"))
}

/// Only reported when the server has the setting (2019+ and Azure SQL list it in sys.database_scoped_configurations)
/// and there is a workload worth looking at.
fn settings_off(snapshot: &Snapshot) -> Vec<SettingOff> {
    if snapshot.server.engine != Engine::Mssql || snapshot.workload.as_ref().is_none_or(|w| w.queries.is_empty()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if off(snapshot, "LAST_QUERY_PLAN_STATS") {
        // Trace flag 2451 keeps them server-wide whatever this setting says; the scan reads no trace flags. Only a
        // server you manage takes it: Azure SQL Database has no trace flags, and the other managed services allow only
        // their own list, so there the sentence would point at something that can't be set.
        out.push(SettingOff::LastQueryPlanStats { trace_flag: !snapshot.server.is_managed() });
    }
    if off(snapshot, "LIGHTWEIGHT_QUERY_PROFILING") {
        out.push(SettingOff::LightweightQueryProfiling);
    }
    out
}

fn setting_off_finding(_snapshot: &Snapshot, off: SettingOff) -> Finding {
    let (key, setting) = match off {
        SettingOff::LastQueryPlanStats { .. } => ("last-query-plan-stats", "LAST_QUERY_PLAN_STATS"),
        SettingOff::LightweightQueryProfiling => ("lightweight-query-profiling", "LIGHTWEIGHT_QUERY_PROFILING"),
    };
    let mut f = finding("last-actual-plans", key, Category::Configuration, subject(SubjectKind::Setting, setting));
    f.severity = Severity::Low;
    f.confidence = Confidence::High;
    f.impact.score = base_score(Severity::Low) * 0.6;
    f.impact.label = t!("check.last_actual_plans.impact");
    match off {
        SettingOff::LastQueryPlanStats { trace_flag } => {
            f.title = t!("check.last_actual_plans.last_query_plan_stats.title");
            let mut summary = vec![t!("check.last_actual_plans.last_query_plan_stats.summary")];
            if trace_flag {
                summary.push(t!("check.last_actual_plans.last_query_plan_stats.trace_flag"));
            }
            f.summary = summary.join(" ");
            f.fix = Some(Fix {
                summary: t!("check.last_actual_plans.last_query_plan_stats.fix"),
                sql: Some("ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;".into()),
                cost: Some(t!("check.last_actual_plans.last_query_plan_stats.fix_cost")),
            });
            f.verify = vec![t!("check.last_actual_plans.last_query_plan_stats.verify")];
        }
        SettingOff::LightweightQueryProfiling => {
            f.title = t!("check.last_actual_plans.lightweight_query_profiling.title");
            f.summary = t!("check.last_actual_plans.lightweight_query_profiling.summary");
            f.fix = Some(Fix {
                summary: t!("check.last_actual_plans.lightweight_query_profiling.fix"),
                sql: Some("ALTER DATABASE SCOPED CONFIGURATION SET LIGHTWEIGHT_QUERY_PROFILING = ON;".into()),
                cost: Some(t!("check.last_actual_plans.lightweight_query_profiling.fix_cost")),
            });
            f.verify = vec![t!("check.last_actual_plans.lightweight_query_profiling.verify")];
        }
    }
    f
}

/// The findings the check makes of `snapshot`.
#[cfg(test)]
fn last_actual_plans(snapshot: &Snapshot) -> Vec<Finding> {
    settings_off(snapshot).into_iter().map(|off| setting_off_finding(snapshot, off)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(name: &str) -> Snapshot {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{name}"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn explains_what_last_query_plan_stats_unlocks() {
        let s = snapshot("mssql-demo.json");
        let found = last_actual_plans(&s);
        assert_eq!(found.len(), 1, "the demo has LAST_QUERY_PLAN_STATS off and lightweight profiling on");
        assert_eq!(found[0].id, "last-actual-plans:last-query-plan-stats");
        assert!(found[0].fix.as_ref().unwrap().sql.as_deref().unwrap().contains("LAST_QUERY_PLAN_STATS = ON"));
        assert!(found[0].summary.contains("Trace flag 2451"), "a server you manage can set it: {}", found[0].summary);
        assert!(found[0].summary.contains("eligible cached"), "{}", found[0].summary);
        assert!(
            found[0].summary.contains("Simple transactional queries may retain only the root operator"),
            "{}",
            found[0].summary
        );
        assert!(found[0].verify[0].contains("when available"), "{:?}", found[0].verify);
        assert!(found[0].verify[0].contains("uncached or evicted"), "{:?}", found[0].verify);
        assert!(last_actual_plans(&snapshot("postgres-demo.json")).is_empty());
    }

    // Azure SQL Database has no trace flags: the finding doesn't point at one.
    #[test]
    fn a_trace_flag_is_mentioned_only_where_one_can_be_set() {
        let mut s = snapshot("mssql-demo.json");
        s.server.platform = Some(culprit_core::Platform::AzureSqlDatabase);
        let found = last_actual_plans(&s);
        assert!(!found[0].summary.contains("Trace flag"), "{}", found[0].summary);
        assert!(found[0].fix.as_ref().unwrap().sql.as_deref().unwrap().contains("LAST_QUERY_PLAN_STATS = ON"));
    }
}
