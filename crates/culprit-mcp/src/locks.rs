//! Compact views for the `blocking` and `deadlocks` tools. Database text (statements, names) is returned as data.

use culprit_core::{BlockNode, DeadlockReport, LiveState};
use serde_json::{Value, json};

const SQL_CLIP: usize = 300;

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SQL_CLIP {
        flat
    } else {
        format!("{}…", flat.chars().take(SQL_CLIP).collect::<String>())
    }
}

fn round(v: f64) -> f64 {
    v.round()
}

fn node(live: &LiveState, n: &BlockNode) -> Value {
    let s = live.sessions.iter().find(|s| s.id == n.session);
    json!({
        "session": n.session,
        "state": s.map(|s| s.state.clone()),
        "user": s.and_then(|s| s.user.clone()),
        "host": s.and_then(|s| s.host.clone()),
        "application": s.and_then(|s| s.application.clone()),
        "waits_for": n.waits_for,
        "wait_type": s.and_then(|s| s.wait.clone()),
        "wait_ms": s.and_then(|s| s.wait_ms).map(round),
        "elapsed_ms": s.and_then(|s| s.elapsed_ms).map(round),
        "transaction_ms": s.and_then(|s| s.transaction_ms).map(round),
        "sql": s.and_then(|s| s.sql.as_deref()).map(clip),
        "blocks": n.children.iter().map(|c| node(live, c)).collect::<Vec<_>>(),
    })
}

pub fn blocking(live: &LiveState) -> Value {
    let incomplete = live.blocking_incomplete();
    let chains: Vec<_> = live
        .blocking
        .as_ref()
        .into_iter()
        .flat_map(|blocking| &blocking.chains)
        .map(|c| {
            json!({
                "head": c.head,
                "head_kind": c.head_kind,
                "headline": c.headline,
                "advice": c.advice,
                "waiters": c.waiters,
                "longest_wait_ms": c.longest_wait_ms.map(round),
                "tree": node(live, &c.tree),
            })
        })
        .collect();
    let mut response = json!({
        "taken_at": live.taken_at,
        "waiting": live.blocking.as_ref().map_or(0, |blocking| blocking.waiting),
        "chains": chains,
        "incomplete": incomplete,
        "notes": live.notes,
    });
    if live.blocking.is_none() {
        response["summary"] = json!(if incomplete {
            culprit_core::t!("cli.blocking.incomplete")
        } else {
            "Nothing is blocked.".to_string()
        });
    }
    response
}

pub fn deadlocks(report: &DeadlockReport) -> Value {
    json!({
        "source": report.source,
        "since_hours": report.since_hours,
        "count": report.deadlocks.len(),
        "counted": report.counted,
        "counted_since": report.counted_since,
        "note": report.note,
        "deadlocks": report.deadlocks.iter().take(20).map(|d| json!({
            "id": d.id,
            "at": d.at,
            "pattern": d.pattern,
            "objects": d.objects,
            "headline": d.headline,
            "explanation": d.explanation,
            "processes": d.processes.iter().map(|p| json!({
                "session": p.session,
                "rolled_back": p.victim,
                "application": p.application,
                "login": p.login,
                "isolation": p.isolation,
                "statement": p.statement.as_deref().map(clip),
            })).collect::<Vec<_>>(),
            "locks": d.resources.iter().map(|r| json!({
                "kind": r.kind,
                "object": r.object,
                "index": r.index,
                "held_by": r.holders.iter().map(|h| json!({ "process": h.process, "mode": h.mode })).collect::<Vec<_>>(),
                "wanted_by": r.waiters.iter().map(|h| json!({ "process": h.process, "mode": h.mode })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "fixes": d.fixes,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn live() -> LiveState {
        LiveState {
            taken_at: "2026-11-12T00:00:00Z".into(),
            notes: vec![],
            sessions: vec![],
            waits: vec![],
            session_actions: vec![],
            session_actions_note: None,
            blocking: None,
        }
    }
    fn session(id: &str, blocked_by: Option<&str>) -> culprit_core::Session {
        serde_json::from_value(json!({"id":id,"state":"waiting","blocked_by":blocked_by,"waiting_on_lock":true}))
            .unwrap()
    }
    #[test]
    fn complete_empty_observation_is_distinct_from_missing_permissions() {
        culprit_core::i18n::scope("en", || {
            let mut live = live();
            let known = blocking(&live);
            assert_eq!(known["incomplete"], false);
            assert_eq!(known["notes"], json!([]));
            assert_eq!(known["summary"], "Nothing is blocked.");
            live.notes.push("Lock permissions are missing.".into());
            let incomplete = blocking(&live);
            assert_eq!(incomplete["incomplete"], true);
            assert_eq!(incomplete["notes"], json!(live.notes));
            assert_eq!(incomplete["waiting"], 0);
            assert_ne!(incomplete["summary"], known["summary"]);
        });
    }
    #[test]
    fn an_unresolved_wait_does_not_report_a_healthy_empty_graph() {
        let mut live = live();
        live.sessions.push(session("51", None));
        let response = blocking(&live);
        assert_eq!(response["incomplete"], true);
        assert_ne!(response["summary"], "Nothing is blocked.");
    }
    #[test]
    fn established_chains_keep_partial_coverage_and_notes() {
        let mut live = live();
        live.sessions = vec![session("51", Some("52")), session("52", None)];
        live.notes.push("Other lock owners are not visible.".into());
        live.blocking = culprit_core::blocking::analyze(culprit_core::Engine::Mssql, &live.sessions, &[], &[]);
        let response = blocking(&live);
        assert_eq!(response["waiting"], 1);
        assert_eq!(response["chains"].as_array().unwrap().len(), 1);
        assert_eq!(response["incomplete"], true);
        assert_eq!(response["notes"], json!(live.notes));
    }
}
