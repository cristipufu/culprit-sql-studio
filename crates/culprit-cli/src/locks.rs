//! `culprit blocking` and `culprit deadlocks`: who blocks whom right now, and recent deadlocks explained.

use std::io::Read;
use std::path::PathBuf;

use clap::Args;
use culprit_core::{BlockNode, Deadlock, DeadlockReport, Error, LiveState, Result, Session, format, t};
use culprit_engine::{Workspace, deadlocks};
use serde_json::{Value, json};

use crate::output::{Format, Out};

#[derive(Args)]
pub struct DeadlocksArgs {
    /// Connection to read deadlocks from. Omit it with --file.
    pub connection: Option<String>,
    /// How far back: 90m, 24h, 7d.
    #[arg(long, default_value = "24h")]
    pub since: String,
    /// Explain a saved deadlock instead: a SQL Server .xdl file, InnoDB status text or PostgreSQL log lines (`-` reads stdin).
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

/// `90m`, `24h`, `7d`, or a bare number of hours.
pub fn parse_since(text: &str) -> Result<f64> {
    let t = text.trim().to_ascii_lowercase();
    let (number, unit) = t.split_at(t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len()));
    let n: f64 = number
        .trim()
        .parse()
        .map_err(|_| Error::invalid(t!("cli.since.unreadable", text = text)).with_hint(t!("cli.since.hint")))?;
    let hours = match unit {
        "" | "h" | "hour" | "hours" => n,
        "m" | "min" | "minutes" => n / 60.0,
        "d" | "day" | "days" => n * 24.0,
        _ => return Err(Error::invalid(t!("cli.since.unit", unit = unit)).with_hint(t!("cli.since.hint"))),
    };
    if hours.is_nan() || hours <= 0.0 {
        return Err(Error::invalid(t!("cli.since.positive")));
    }
    Ok(hours)
}

pub async fn blocking(ws: &Workspace, connection: &str, out: &Out) -> Result<u8> {
    let live = ws.live(connection).await?;
    if out.format() == Format::Json {
        out.value(&blocking_json(&live));
        return Ok(0);
    }
    let Some(blocking) = live.blocking.as_ref().filter(|b| !b.chains.is_empty()) else {
        if live.blocking_incomplete() {
            println!("{}", t!("cli.blocking.incomplete"));
            for note in &live.notes {
                println!("{}", t!("cli.live.limitation", note = note));
            }
            return Ok(0);
        }
        let active = live.sessions.iter().filter(|s| s.state != "idle").count();
        println!("{}", t!("cli.blocking.none", n = active, count = active.to_string()));
        return Ok(0);
    };
    for chain in &blocking.chains {
        println!("{}", out.bold(&chain.headline));
        print_node(&live, &chain.tree, out, 0);
        println!("  {}", out.dim(&chain.advice));
        println!();
    }
    Ok(0)
}

fn session<'a>(live: &'a LiveState, id: &str) -> Option<&'a Session> {
    live.sessions.iter().find(|s| s.id == id)
}

fn print_node(live: &LiveState, node: &BlockNode, out: &Out, depth: usize) {
    let s = session(live, &node.session);
    let prefix = if depth == 0 { "  ".to_string() } else { format!("{}└─ ", "   ".repeat(depth)) };
    let state = match (depth, s) {
        (0, Some(s)) => s.state.clone(),
        (_, Some(s)) => match s.wait_ms.or(s.elapsed_ms) {
            Some(ms) => t!("cli.blocking.waits", duration = format::duration(ms)),
            None => t!("cli.blocking.waiting"),
        },
        (_, None) => t!("cli.blocking.not_visible"),
    };
    let lock = node.waits_for.clone().unwrap_or_default();
    let who = s
        .map(|s| {
            let user = s.user.clone().unwrap_or_default();
            let host = s.host.as_deref().map(|h| format!("@{h}")).unwrap_or_default();
            let app = s.application.as_deref().map(|a| format!(" · {a}")).unwrap_or_default();
            format!("{user}{host}{app}")
        })
        .unwrap_or_default();
    let sql: String = s
        .and_then(|s| s.sql.as_deref())
        .map(|q| q.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let sql = if sql.chars().count() > 90 { format!("{}…", sql.chars().take(89).collect::<String>()) } else { sql };
    let detail = [state, lock, who].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ");
    println!("{prefix}{}  {}  {}", out.bold(&node.session), detail, out.dim(&sql));
    for child in &node.children {
        print_node(live, child, out, depth + 1);
    }
}

fn blocking_json(live: &LiveState) -> Value {
    let Some(b) = live.blocking.as_ref() else {
        return json!({ "taken_at": live.taken_at, "waiting": 0, "chains": [], "incomplete": live.blocking_incomplete(), "notes": live.notes });
    };
    let mut ids = Vec::new();
    fn walk(node: &BlockNode, ids: &mut Vec<String>) {
        ids.push(node.session.clone());
        for c in &node.children {
            walk(c, ids);
        }
    }
    for c in &b.chains {
        walk(&c.tree, &mut ids);
    }
    let sessions: Vec<&Session> = live.sessions.iter().filter(|s| ids.contains(&s.id)).collect();
    json!({ "taken_at": live.taken_at, "waiting": b.waiting, "chains": b.chains, "sessions": sessions, "incomplete": live.blocking_incomplete(), "notes": live.notes })
}

pub async fn deadlocks(ws: &Workspace, args: DeadlocksArgs, out: &Out) -> Result<u8> {
    let report = match (&args.file, &args.connection) {
        (Some(path), _) => {
            let text = if path.as_os_str() == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).map_err(|e| Error::invalid(e.to_string()))?;
                buf
            } else {
                std::fs::read_to_string(path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?
            };
            DeadlockReport {
                deadlocks: deadlocks::parse_deadlock(&text)?,
                source: t!("cli.deadlocks.from_file"),
                ..Default::default()
            }
        }
        (None, Some(connection)) => ws.deadlocks(connection, parse_since(&args.since)?).await?,
        (None, None) => return Err(Error::invalid(t!("cli.deadlocks.name_one"))),
    };
    if out.format() == Format::Json {
        out.value(&serde_json::to_value(&report).unwrap_or(Value::Null));
        return Ok(0);
    }
    println!("{}", count_line(report.deadlocks.len(), report.since_hours, &report.source));
    if let Some(note) = &report.note {
        println!("{}", out.dim(note));
    }
    for d in &report.deadlocks {
        println!();
        print_deadlock(d, out);
    }
    Ok(0)
}

/// "2 deadlocks in the last 24 hours (Query Store).": how many deadlocks were found, over what and where.
fn count_line(n: usize, since_hours: f64, source: &str) -> String {
    let count = n.to_string();
    let window = (since_hours > 0.0).then(|| hours_phrase(since_hours));
    match (window, source.is_empty()) {
        (None, true) => t!("cli.deadlocks.count", n = n, count = count),
        (None, false) => t!("cli.deadlocks.count_source", n = n, count = count, source = source),
        (Some(window), true) => t!("cli.deadlocks.count_window", n = n, count = count, window = window),
        (Some(window), false) => {
            t!("cli.deadlocks.count_window_source", n = n, count = count, window = window, source = source)
        }
    }
}

/// "the last 24 hours": the period read, worded whole per unit. Whole days from two days, whole hours from one, minutes
/// under that; not `culprit_core::last_span`, which rounds a period differently.
fn hours_phrase(hours: f64) -> String {
    if hours >= 48.0 && (hours / 24.0).fract() == 0.0 {
        let days = hours / 24.0;
        t!("cli.deadlocks.last_days", n = days, count = format!("{days:.0}"))
    } else if hours >= 1.0 {
        t!("cli.deadlocks.last_hours", n = hours.round(), count = format!("{hours:.0}"))
    } else {
        let minutes = hours * 60.0;
        t!("cli.deadlocks.last_minutes", n = minutes.round(), count = format!("{minutes:.0}"))
    }
}

fn print_deadlock(d: &Deadlock, out: &Out) {
    let when = d.at.as_deref().map(|t| format!("{} ", t.replace('T', " ").trim_end_matches('Z'))).unwrap_or_default();
    println!("{}{}", out.dim(&when), out.bold(&d.headline));
    println!("  {}", d.explanation);
    for p in &d.processes {
        let rolled_back = t!("cli.deadlocks.rolled_back");
        let tag = if p.victim { out.red(&rolled_back) } else { " ".repeat(rolled_back.chars().count()) };
        let who = [
            p.session.as_deref().map(|s| t!("cli.deadlocks.session", session = s)),
            p.application.clone(),
            p.login.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let sql: String =
            p.statement.as_deref().map(|q| q.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let sql =
            if sql.chars().count() > 100 { format!("{}…", sql.chars().take(99).collect::<String>()) } else { sql };
        println!("  {tag}  {who}");
        if !sql.is_empty() {
            println!("               {}", out.dim(&sql));
        }
    }
    for (i, fix) in d.fixes.iter().enumerate() {
        println!("  {}. {}: {}", i + 1, out.bold(&fix.title), fix.detail);
        if let Some(sql) = &fix.sql {
            println!("     {sql}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_like_people_write_them() {
        assert_eq!(parse_since("24h").unwrap(), 24.0);
        assert_eq!(parse_since("90m").unwrap(), 1.5);
        assert_eq!(parse_since("7d").unwrap(), 168.0);
        assert_eq!(parse_since("6").unwrap(), 6.0);
        assert!(parse_since("soon").is_err());
        assert!(parse_since("0h").is_err());
        assert_eq!(hours_phrase(24.0), "the last 24 hours");
        assert_eq!(hours_phrase(168.0), "the last 7 days");
        // How many were found, over what and where, as it was said before its words moved into the catalog.
        assert_eq!(count_line(0, 0.0, ""), "No deadlocks.");
        assert_eq!(count_line(0, 0.0, "file"), "No deadlocks (file).");
        assert_eq!(count_line(0, 24.0, "file"), "No deadlocks in the last 24 hours (file).");
        assert_eq!(count_line(1, 168.0, "Query Store"), "1 deadlock in the last 7 days (Query Store).");
        assert_eq!(count_line(2, 1.0, ""), "2 deadlocks in the last 1 hours.");
        assert_eq!(count_line(2, 0.5, ""), "2 deadlocks in the last 30 minutes.");
        assert_eq!(count_line(12, 50.0, ""), "12 deadlocks in the last 50 hours.");
    }
}
