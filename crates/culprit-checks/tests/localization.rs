use culprit_core::{RefinementOutcome, Report, Snapshot, i18n};
use serde_json::{Value, json};

/// Compare the decisions clients act on, retaining finding order (rank), evidence, measurements and executable SQL.
fn decisions(report: &Report) -> Value {
    json!({
        "findings": report.findings.iter().map(|finding| {
            json!({
                "id": finding.id, "check": finding.check, "category": finding.category,
                "severity": finding.severity, "actionability": finding.actionability,
                "confidence": finding.confidence, "subject": finding.subject, "facts": finding.facts,
                "inactive": finding.inactive, "score": finding.impact.score,
                "share": finding.impact.db_time_share, "per_day": finding.impact.time_ms_per_day,
                "queries": finding.queries.iter().map(|q| json!({"id": q.id, "node": q.plan_node})).collect::<Vec<_>>(),
                "sql": finding.fix.as_ref().and_then(|fix| fix.sql.as_ref()),
                "metrics": finding.metrics.iter().map(|m| json!({"key": m.key, "value": m.value, "unit": m.unit})).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "locked": report.locked.iter().map(|locked| json!({"id": locked.check.id, "missing": locked.missing})).collect::<Vec<_>>(),
        "coverage": report.assessments.iter().map(|a| json!({"check": a.check, "status": a.status, "queries": a.assessed_queries})).collect::<Vec<_>>(),
        "refinement": report.refinement.iter().filter(|d| !matches!(d.outcome, RefinementOutcome::Presentation {..})).collect::<Vec<_>>(),
    })
}

#[test]
fn changing_language_preserves_rank_evidence_and_executable_actions_for_every_provider() {
    let fixtures = [
        include_str!("../../../fixtures/mssql-demo.json"),
        include_str!("../../../fixtures/postgres-demo.json"),
        include_str!("../../../fixtures/mysql-demo.json"),
        include_str!("../../../fixtures/sqlite-demo.json"),
        include_str!("../../../fixtures/yugabyte-demo.json"),
        include_str!("../../../fixtures/timescale-demo.report.json"),
    ];
    for fixture in fixtures {
        let mut value: Value = serde_json::from_str(fixture).unwrap();
        if value.get("snapshot").is_some() {
            value = value["snapshot"].take();
        }
        let snapshot: Snapshot = serde_json::from_value(value).unwrap();
        let en = i18n::scope("en", || culprit_checks::analyze(snapshot.clone()));
        let expected = decisions(&en);
        for language in ["es", "fr", "de"] {
            let translated = i18n::scope(language, || culprit_checks::analyze(snapshot.clone()));
            assert_eq!(decisions(&translated), expected, "{} in {language}", snapshot.server.engine);
            assert_ne!(translated.summary.headline, en.summary.headline, "the catalog was used");
        }
    }
}

#[test]
fn french_read_counts_only_say_once_for_exactly_one() {
    i18n::scope("fr", || {
        for (count, digits, words) in
            [(0.0, "0", "0 fois"), (0.5, "0,5", "0,5 fois"), (1.0, "1", "une fois"), (2.0, "2", "2 fois")]
        {
            let said = culprit_core::t!(
                "check.overlapping_index.summary.reads",
                index = "narrow",
                n = count,
                reads = digits,
                since = "depuis le démarrage",
                wider = "wide",
                wider_n = 1,
                wider_reads = "1"
            );
            assert_eq!(said, format!("narrow a été lu {words} depuis le démarrage, et wide une fois."));
        }
    });
}
