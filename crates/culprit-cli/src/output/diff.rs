use culprit_core::changes::{Period, short_time};
use culprit_core::{ChangeKind, ChangeStatus, Changes, Drift, DriftKind, DriftPresence, PlanDiff, format, t};
use serde_json::json;

use super::{BLUE, BOLD, DIM, Format, GREEN, Out, RED, YELLOW, environment_word};

impl Out {
    pub fn plan_diff(&self, diff: &PlanDiff, name_a: &str, name_b: &str) {
        if self.format == Format::Json {
            let mut value = json!(diff);
            value["a"] = json!(name_a);
            value["b"] = json!(name_b);
            return self.json(&value);
        }
        for headline in &diff.headlines {
            println!("{}", self.paint(BOLD, headline));
        }
        let cost = |c: Option<f64>| match c {
            Some(c) if c < 10.0 => format!("{c:.3}"),
            Some(c) => format::count(c),
            None => t!("engine.export.unavailable"),
        };
        let sides =
            t!("cli.plan_diff.sides", a = name_a, cost_a = cost(diff.cost_a), b = name_b, cost_b = cost(diff.cost_b));
        println!("{}", self.paint(DIM, &sides));
        if diff.changes.is_empty() {
            return;
        }
        println!();
        for change in &diff.changes {
            let mark = match change.kind {
                ChangeKind::Access | ChangeKind::Operator => self.paint(YELLOW, "~"),
                ChangeKind::Added => self.paint(GREEN, "+"),
                ChangeKind::Removed => self.paint(RED, "-"),
                ChangeKind::Cost => self.paint(BLUE, "$"),
                ChangeKind::Rows => self.paint(BLUE, "#"),
                ChangeKind::Warning => self.paint(YELLOW, "!"),
            };
            let detail = change.detail.as_deref().map(|d| self.paint(DIM, &format!("  {d}"))).unwrap_or_default();
            println!("{mark} {}{detail}", change.title);
        }
    }

    pub fn drift(&self, drift: &Drift) {
        if self.format == Format::Json {
            return self.json(&json!(drift));
        }
        if self.format == Format::Sarif {
            return self.json(&super::sarif::drift_sarif(drift));
        }
        let side = |s: &culprit_core::DriftSide| {
            format!("{} ({}, {})", s.connection, environment_word(s.environment), s.server())
        };
        println!("{}  vs  {}", self.paint(BOLD, &side(&drift.a)), self.paint(BOLD, &side(&drift.b)));
        println!("{}\n", drift.summary);
        for (kind, heading) in [
            (DriftKind::Index, t!("cli.drift.indexes")),
            (DriftKind::Table, t!("cli.drift.tables")),
            (DriftKind::Column, t!("cli.drift.columns")),
            (DriftKind::Setting, t!("cli.drift.settings")),
            (DriftKind::Version, t!("cli.drift.version")),
        ] {
            let items: Vec<_> = drift.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            println!("{}", self.paint(BOLD, &heading));
            for item in items {
                let mark = match item.presence {
                    DriftPresence::OnlyA => self.paint(YELLOW, "◀"),
                    DriftPresence::OnlyB => self.paint(YELLOW, "▶"),
                    DriftPresence::Different => self.paint(RED, "≠"),
                    DriftPresence::Renamed => self.paint(DIM, "~"),
                };
                println!("  {mark} {}", item.title);
                if let (Some(sql), Some(target)) = (&item.sql, &item.sql_for) {
                    println!("{}", self.paint(DIM, &format!("      {}", t!("cli.drift.on", target = target))));
                    for line in sql.lines() {
                        println!("      {}", self.paint(GREEN, line));
                    }
                } else {
                    match (&item.a, &item.b) {
                        (Some(before), Some(after))
                            if !item.title.contains(before.as_str()) || !item.title.contains(after.as_str()) =>
                        {
                            println!("      {}", self.paint(DIM, &format!("{before} → {after}")));
                        }
                        (Some(detail), None) | (None, Some(detail)) if !item.title.contains(detail.as_str()) => {
                            println!("      {}", self.paint(DIM, detail));
                        }
                        _ => {}
                    }
                }
            }
            println!();
        }
    }

    pub fn changes(&self, c: &Changes) {
        if self.format == Format::Json {
            return self.json(&json!(c));
        }
        let span = |p: &Period| format!("{} → {}", short_time(&p.start), short_time(&p.end));
        println!("{}", self.paint(BOLD, &t!("cli.changes.title", connection = &c.connection)));
        let periods = t!("cli.changes.periods", recent = span(&c.recent), baseline = span(&c.baseline));
        println!("{}", self.paint(DIM, &periods));
        println!("{}\n", c.summary);
        // A note the summary already says (why there's nothing to compare) isn't repeated.
        let notes: Vec<&String> = c.notes.iter().filter(|n| !c.summary.contains(n.as_str())).collect();
        for note in &notes {
            println!("{}", self.paint(DIM, note));
        }
        if !notes.is_empty() {
            println!();
        }
        let shown: Vec<_> = c.events.iter().filter(|e| !e.minor).collect();
        if !shown.is_empty() {
            println!("{}", self.paint(BOLD, &t!("cli.changes.around")));
            for e in shown.iter().take(12) {
                println!("  {}  {}", e.title, self.paint(DIM, &e.when()));
            }
            if shown.len() > 12 {
                let more = shown.len() - 12;
                println!("  {}", self.paint(DIM, &t!("cli.more_json", n = more, count = more.to_string())));
            }
            println!();
        }
        for (status, heading, style) in [
            (ChangeStatus::Regressed, t!("cli.changes.slower"), RED),
            (ChangeStatus::New, t!("cli.changes.new"), YELLOW),
            (ChangeStatus::Improved, t!("cli.changes.faster"), GREEN),
            (ChangeStatus::Stopped, t!("cli.stopped_running"), BLUE),
            (ChangeStatus::NotComparable, t!("cli.changes.not_comparable"), DIM),
            (ChangeStatus::LowSample, t!("cli.changes.too_few"), DIM),
        ] {
            let items: Vec<_> = c.queries.iter().filter(|q| q.status == status).collect();
            if items.is_empty() {
                continue;
            }
            println!("{} {}", self.paint(style, "●"), self.paint(BOLD, &format!("{heading} ({})", items.len())));
            for q in items.iter().take(12) {
                let text: String = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
                let text = if text.chars().count() > 100 {
                    format!("{}…", text.chars().take(99).collect::<String>())
                } else {
                    text
                };
                println!("  {}  {}", self.paint(DIM, &format!("{:>8}", q.query_id)), q.headline);
                println!("            {}", self.paint(DIM, &text));
            }
            if items.len() > 12 {
                let more = items.len() - 12;
                println!("  {}", self.paint(DIM, &t!("cli.more_json", n = more, count = more.to_string())));
            }
            println!();
        }
        if c.unchanged > 0 {
            let same = t!("cli.changes.same", n = c.unchanged, count = c.unchanged.to_string());
            println!("{}", self.paint(DIM, &same));
        }
    }
}
