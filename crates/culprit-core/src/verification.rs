//! Did the fix work? Compare the report from before a change with a fresh one, finding by finding: fixed, still there,
//! can't tell (the check couldn't run), and what's new. Pure, so the CLI, the app and agents read the same answer.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::finding::{Finding, Report};
use crate::{format, t};

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VerifyStatus {
    /// In the earlier scan, gone from the new one, and the check ran.
    Fixed,
    /// Still reported by the new scan.
    StillPresent,
    /// The check couldn't run in the new scan, or the id is in neither scan.
    Unknown,
    /// Only the new scan reports it.
    New,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct VerifiedFinding {
    pub id: String,
    pub title: String,
    pub status: VerifyStatus,
    /// Impact in the earlier scan ("31% of DB time"), when it was there.
    pub before: Option<String>,
    /// Impact in the new scan, when it is still there.
    pub after: Option<String>,
    /// What to make of it: why a fixed query can still show up, what couldn't be checked.
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct Verification {
    pub connection: String,
    pub before_taken_at: String,
    pub after_taken_at: String,
    /// One sentence: "2 of 3 fixed; 1 still present. 1 new finding."
    pub summary: String,
    pub findings: Vec<VerifiedFinding>,
    /// Findings only the new scan has (a new index that isn't used yet, a regression the change caused).
    pub new_findings: Vec<VerifiedFinding>,
}

/// Compare `after` (a fresh scan) with `before`. `ids` picks the findings to check; empty means every finding `before`
/// had.
pub fn compare_reports(before: &Report, after: &Report, ids: &[String]) -> Verification {
    let targets: Vec<String> = if ids.is_empty() {
        before.findings.iter().filter(|f| f.is_actionable()).map(|f| f.id.clone()).collect()
    } else {
        ids.to_vec()
    };
    let find = |report: &'_ Report, id: &str| report.findings.iter().find(|f| f.id == id).cloned();
    let identity = before.snapshot.database_comparison(&after.snapshot);
    let measurement = before.snapshot.report_comparison(&after.snapshot);
    let mut findings = Vec::new();
    for id in &targets {
        let (was, now) = (find(before, id), find(after, id));
        let query_evidence = was
            .as_ref()
            .or(now.as_ref())
            .is_some_and(|f| !f.queries.is_empty() || f.subject.kind == crate::SubjectKind::Query);
        let incompatible = identity.reason().or_else(|| {
            (query_evidence && before.snapshot.workload.is_some() && after.snapshot.workload.is_some())
                .then(|| measurement.reason())
                .flatten()
        });
        if let Some(reason) = incompatible {
            findings.push(VerifiedFinding {
                id: id.clone(),
                title: now.as_ref().or(was.as_ref()).map(|f| f.title.clone()).unwrap_or_else(|| id.clone()),
                status: VerifyStatus::Unknown,
                before: was.map(|f| f.impact.label),
                after: now.map(|f| f.impact.label),
                note: Some(reason.explanation()),
            });
            continue;
        }
        let item = match (was, now) {
            (Some(was), Some(now)) => VerifiedFinding {
                id: id.clone(),
                title: now.title.clone(),
                status: VerifyStatus::StillPresent,
                before: Some(was.impact.label.clone()),
                after: Some(now.impact.label.clone()),
                note: still_present_note(&was, &now, after),
            },
            // A finding about statements can't be confirmed gone by a scan that read no statements (the history was
            // switched off, or the earlier scan read a query log this one didn't).
            (Some(was), None) if !was.queries.is_empty() && before.snapshot.workload.is_some() && after.snapshot.workload.is_none() => {
                VerifiedFinding {
                    id: id.clone(),
                    title: was.title.clone(),
                    status: VerifyStatus::Unknown,
                    before: Some(was.impact.label.clone()),
                    after: None,
                    note: Some(t!(
                        "model.verify.no_query_history",
                        why = after
                            .summary
                            .no_workload
                            .as_deref()
                            .map(|why| why.trim_end_matches('.').to_string())
                            .unwrap_or_else(|| t!("model.verify.no_statement_statistics"))
                    )),
                }
            }
            (Some(was), None) if was.verification_basis == crate::VerificationBasis::CounterChange => VerifiedFinding {
                id: id.clone(), title: was.title.clone(), status: VerifyStatus::Unknown,
                before: Some(was.impact.label.clone()), after: None,
                note: Some(t!("model.verify.cumulative_counters")),
            },
            (Some(was), None) => match after.finding_absence_problem_with_baseline(
                &was.id, &was.check, &was.queries.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), was.facts.plan_baseline.as_ref()
            ) {
                Some(missing) => VerifiedFinding {
                    id: id.clone(),
                    title: was.title.clone(),
                    status: VerifyStatus::Unknown,
                    before: Some(was.impact.label.clone()),
                    after: None,
                    note: Some([t!("model.verify.cannot_establish_fixed"), missing].join(" ")),
                },
                None => VerifiedFinding {
                    id: id.clone(),
                    title: was.title.clone(),
                    status: VerifyStatus::Fixed,
                    before: Some(was.impact.label.clone()),
                    after: None,
                    note: None,
                },
            },
            (None, Some(now)) => VerifiedFinding {
                id: id.clone(),
                title: now.title.clone(),
                status: VerifyStatus::StillPresent,
                before: None,
                after: Some(now.impact.label.clone()),
                note: Some(t!("model.verify.newly_reported")),
            },
            (None, None) => VerifiedFinding {
                id: id.clone(),
                title: id.clone(),
                status: VerifyStatus::Unknown,
                before: None,
                after: None,
                note: Some(
                    "Neither the latest scan nor the new one reports it: it may have been fixed before the latest scan. \
                     Current ids come from `culprit findings`."
                        .into(),
                ),
            },
        };
        findings.push(item);
    }
    let new_findings: Vec<VerifiedFinding> = after
        .findings
        .iter()
        .filter(|f| {
            identity.allows_comparison()
                && (measurement.allows_comparison()
                    || (f.queries.is_empty() && f.subject.kind != crate::SubjectKind::Query))
                && f.is_actionable()
                && !before.findings.iter().any(|b| b.id == f.id)
                && !targets.contains(&f.id)
        })
        .map(|f| VerifiedFinding {
            id: f.id.clone(),
            title: f.title.clone(),
            status: VerifyStatus::New,
            before: None,
            after: Some(f.impact.label.clone()),
            note: None,
        })
        .collect();
    Verification {
        connection: after.snapshot.connection.clone(),
        before_taken_at: before.snapshot.taken_at.clone(),
        after_taken_at: after.snapshot.taken_at.clone(),
        summary: summary(&findings, new_findings.len()),
        findings,
        new_findings,
    }
}

fn still_present_note(was: &Finding, now: &Finding, after: &Report) -> Option<String> {
    if !now.is_actionable() {
        let kind = match now.actionability {
            crate::Actionability::History => "history",
            _ => "information",
        };
        return Some(t!("model.verify.retained_as_context", kind = kind));
    }
    if !now.queries.is_empty()
        && let Some(w) = after.snapshot.workload.as_ref()
    {
        let span = format::duration(w.measurement_hours() * 3_600_000.0);
        return Some(if let Some(log) = &w.log {
            t!("model.verify.still_in_the_log", log = log)
        } else if w.is_windowed() {
            t!("model.verify.still_in_the_window", span = span)
        } else {
            // A week moves a years-long total by a hair: the difference between two scans shows the change.
            // The connection's name in the command, or its placeholder (command syntax, said as it is).
            let connection =
                if after.snapshot.connection.trim().is_empty() { "<connection>" } else { &after.snapshot.connection };
            t!("model.verify.still_in_the_totals", source = &w.source, span = span, connection = connection)
        });
    }
    let (b, a) = (was.impact.score, now.impact.score);
    (b > 0.0 && a < b * 0.5).then(|| t!("model.verify.smaller", before = &was.impact.label, now = &now.impact.label))
}

fn summary(findings: &[VerifiedFinding], new: usize) -> String {
    let n = |s: VerifyStatus| findings.iter().filter(|f| f.status == s).count();
    let (fixed, still, unknown) = (n(VerifyStatus::Fixed), n(VerifyStatus::StillPresent), n(VerifyStatus::Unknown));
    let mut out = if findings.len() == 1 {
        match findings[0].status {
            VerifyStatus::Fixed => t!("model.verify.one_fixed"),
            VerifyStatus::StillPresent => t!("model.verify.one_still_present"),
            _ => t!("model.verify.one_unknown"),
        }
    } else {
        t!(
            "model.verify.several",
            fixed = fixed.to_string(),
            total = findings.len().to_string(),
            still = still.to_string(),
            unknown = unknown.to_string()
        )
    };
    if new > 0 {
        out.push(' ');
        out.push_str(&t!("model.verify.new_findings", n = new, count = new.to_string()));
    }
    out
}

#[cfg(test)]
mod counter_verification_tests {
    use super::*;
    #[test]
    fn losing_counters_never_proves_a_fix_on_any_provider() {
        for engine in [crate::Engine::Mssql, crate::Engine::Postgres, crate::Engine::Mysql, crate::Engine::Sqlite] {
            let mut before: Report =
                serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
            before.snapshot.server.engine = engine;
            before.snapshot.identity.as_mut().unwrap().engine = engine;
            before.findings.truncate(1);
            before.findings[0].verification_basis = crate::VerificationBasis::CounterChange;
            let mut after = before.clone();
            after.findings.clear();
            let result = compare_reports(&before, &after, &[]);
            assert_eq!(result.findings[0].status, VerifyStatus::Unknown, "{engine:?}");
            // A schema change with normal rescan evidence can still be confirmed gone.
            before.findings[0].verification_basis = crate::VerificationBasis::Rescan;
            let check = &before.findings[0].check;
            let assessment = after.assessments.iter_mut().find(|a| &a.check == check).unwrap();
            assessment.status = crate::CheckStatus::Assessed;
            assessment.reason = None;
            assert_eq!(compare_reports(&before, &after, &[]).findings[0].status, VerifyStatus::Fixed);
        }
    }

    #[test]
    fn incomplete_or_unregistered_checks_do_not_verify_absence_even_with_other_positive_findings() {
        let mut before: Report =
            serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        before.findings.truncate(1);
        let finding = &mut before.findings[0];
        finding.verification_basis = crate::VerificationBasis::Rescan;
        let check = finding.check.clone();
        let mut after = before.clone();
        after.findings.clear();
        for status in
            [crate::CheckStatus::Unavailable, crate::CheckStatus::InsufficientEvidence, crate::CheckStatus::Findings]
        {
            let assessment = after.assessments.iter_mut().find(|a| a.check == check).unwrap();
            assessment.status = status;
            assessment.reason = Some("Only part of the required source was collected.".into());
            // A global source failure retains no per-query coverage, as the pipeline's evidence evaluator records.
            assessment.assessed_queries.clear();
            assert_eq!(compare_reports(&before, &after, &[]).findings[0].status, VerifyStatus::Unknown);
        }
        after.assessments.retain(|a| a.check != check);
        assert_eq!(compare_reports(&before, &after, &[]).findings[0].status, VerifyStatus::Unknown);
        // Positive evidence of inapplicability (e.g. the last foreign key was dropped) can establish absence.
        after.assessments.push(crate::CheckAssessment {
            check,
            title: String::new(),
            status: crate::CheckStatus::NotApplicable,
            reason: Some("The complete catalog contains no such objects.".into()),
            assessed_queries: Vec::new(),
        });
        assert_eq!(compare_reports(&before, &after, &[]).findings[0].status, VerifyStatus::Fixed);
    }

    // What verifying says, word for word, as English has always said it.
    #[test]
    fn verification_reads_as_english_writes_it() {
        let said = |status: VerifyStatus| VerifiedFinding {
            id: "x".into(),
            title: "x".into(),
            status,
            before: None,
            after: None,
            note: None,
        };
        let mut out = Vec::new();
        for status in [VerifyStatus::Fixed, VerifyStatus::StillPresent, VerifyStatus::Unknown] {
            out.push(summary(&[said(status)], 0));
        }
        out.push(summary(&[said(VerifyStatus::Fixed)], 1));
        out.push(summary(&[said(VerifyStatus::Fixed), said(VerifyStatus::Fixed)], 2));
        out.push(summary(
            &[said(VerifyStatus::Fixed), said(VerifyStatus::StillPresent), said(VerifyStatus::Unknown)],
            0,
        ));
        out.push(summary(&[said(VerifyStatus::Fixed), said(VerifyStatus::Unknown)], 0));
        let before: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let (was, mut now) = (before.findings[0].clone(), before.findings[0].clone());
        out.push(still_present_note(&was, &now, &before).unwrap_or_default());
        let mut totals = before.clone();
        let w = totals.snapshot.workload.as_mut().unwrap();
        (w.windowed, w.source) = (false, "pg_stat_statements".into());
        totals.snapshot.connection = String::new();
        out.push(still_present_note(&was, &now, &totals).unwrap_or_default());
        totals.snapshot.workload.as_mut().unwrap().log = Some("Rails".into());
        out.push(still_present_note(&was, &now, &totals).unwrap_or_default());
        now.queries.clear();
        now.impact.score = was.impact.score * 0.2;
        now.impact.label = "1% of DB time".into();
        out.push(still_present_note(&was, &now, &before).unwrap_or_default());
        for actionability in [crate::Actionability::History, crate::Actionability::Information] {
            now.actionability = actionability;
            out.push(still_present_note(&was, &now, &before).unwrap_or_default());
        }
        let mut after = before.clone();
        after.findings.clear();
        after.snapshot.workload = None;
        after.summary.no_workload = None;
        let ids = vec![was.id.clone()];
        out.push(compare_reports(&before, &after, &ids).findings[0].note.clone().unwrap_or_default());
        after.summary.no_workload = Some("Query Store is OFF on this database.".into());
        out.push(compare_reports(&before, &after, &ids).findings[0].note.clone().unwrap_or_default());
        let mut later = before.clone();
        later.findings.clear();
        out.push(compare_reports(&later, &before, &ids).findings[0].note.clone().unwrap_or_default());
        let window = "It is measured over the last 4 h 34 min of workload, which still includes runs from before the \
                      change. Check again once that window has moved past it, or scan with a shorter window.";
        let totals = "pg_stat_statements keeps running totals since its statistics were reset (4 h 34 min ago), so \
                      runs from before the change still count, and it stays listed. What changed, with Since at the \
                      ship time (`culprit changes <connection> --since <ship time>`), compares the runs since then \
                      with the runs before, from Culprit's scans.";
        let context = "retained as context. This does not demonstrate that a change was verified under representative \
                       load.";
        assert_eq!(
            out,
            [
                "Fixed: the new scan no longer reports it.".to_string(),
                "Still present in the new scan.".into(),
                "Can't tell from the new scan.".into(),
                "Fixed: the new scan no longer reports it. 1 new finding.".into(),
                "2 of 2 fixed. 2 new findings.".into(),
                "1 of 3 fixed; 1 still present; 1 can't be checked.".into(),
                "1 of 2 fixed; 1 can't be checked.".into(),
                window.into(),
                totals.into(),
                "It is measured from the Rails log Culprit read, which still includes runs from before the change. \
                 Scan again with a log written since."
                    .into(),
                "Smaller than before: 100% of runs fail → 1% of DB time.".into(),
                format!("History: {context}"),
                format!("Information: {context}"),
                "The new scan has no query history (no statement statistics), so this can't be confirmed.".into(),
                "The new scan has no query history (Query Store is OFF on this database), so this can't be confirmed."
                    .into(),
                "The earlier scan didn't report it; the new one does.".into(),
            ]
        );
    }
}
