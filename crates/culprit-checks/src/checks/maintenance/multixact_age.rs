//! The `multixact-age` check: PostgreSQL's MultiXact age limit, independent of transaction ID age, which can reject
//! row-locking writes even when ordinary transaction ID age is healthy.

use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::FreezeAge;
use culprit_core::t;

pub fn multixact_age_info() -> CheckInfo {
    CheckInfo {
        id: "multixact-age".into(),
        title: t!("check.multixact_age.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres],
        requires: vec![],
        description: t!("check.multixact_age.info.description"),
    }
}

check!(
    MultixactAgeCheck: multixact_age_info,
    decide = multixact_ages,
    render = multixact_age_finding,
    case<'s> = MultixactAge<'s>,
    evidence = &[Evidence::Observed(Observation::Multixacts, &[culprit_core::source_id::MULTIXACTS])],
);

/// The oldest MultiXact age past the point where vacuum should have advanced it, and how close it is to the limit.
pub struct MultixactAge<'s> {
    oldest: &'s FreezeAge,
    severity: Severity,
}

/// The oldest subject whose MultiXact age is past twice its freeze age (at least 500M, 800M when unknown, never
/// later than 1.2B): one finding for the database.
fn multixact_ages(s: &Snapshot) -> Vec<MultixactAge<'_>> {
    s.operations
        .multixacts
        .iter()
        .filter(|a| {
            a.age.is_finite()
                && a.age
                    >= a.freeze_max_age
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .map(|v| (v * 2.0).max(500_000_000.0))
                        .unwrap_or(800_000_000.0)
                        .min(1_200_000_000.0)
        })
        .max_by(|a, b| a.age.total_cmp(&b.age))
        .map(|oldest| MultixactAge {
            oldest,
            severity: if oldest.age >= 1_200_000_000.0 { Severity::High } else { Severity::Medium },
        })
        .into_iter()
        .collect()
}

fn multixact_age_finding(s: &Snapshot, case: MultixactAge<'_>) -> Finding {
    let age = case.oldest;
    let mut f = finding(
        "multixact-age",
        &s.server.database,
        Category::Maintenance,
        subject(SubjectKind::Database, &s.server.database),
    );
    f.title = t!("check.multixact_age.title", age = format::count(age.age), subject = &age.subject);
    f.summary = t!("check.multixact_age.summary");
    f.severity = case.severity;
    f.confidence = Confidence::High;
    f.impact.score = base_score(f.severity);
    f.impact.label = t!("check.multixact_age.impact", age = format::count(age.age));
    f.metrics = vec![
        Metric::new(t!("check.multixact_age.metric.age"), age.age, Unit::Count),
        Metric::text(t!("check.multixact_age.metric.oldest"), &age.subject),
    ];
    f.fix = Some(Fix {
        summary: t!("check.multixact_age.fix"),
        sql: None,
        cost: Some(t!("check.multixact_age.fix_cost")),
    });
    f.verify = vec![t!("check.multixact_age.verify")];
    f
}
