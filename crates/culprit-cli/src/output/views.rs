use culprit_core::{LiveState, Plan, PlanNode, ResultSet, Severity, format, t};
use serde_json::{Value, json};

use super::{BLUE, BOLD, DIM, Format, GREEN, Out, RED, YELLOW, bar, kb, plan_kind};

impl Out {
    pub fn plan(&self, plan: &Plan) {
        let found = plan.insights.clone().unwrap_or_else(|| culprit_core::insights::PlanInsights {
            scan_context: Some(false),
            items: culprit_core::insights::insights(plan),
            note: culprit_core::insights::coverage_note(plan),
        });
        let summary = culprit_core::actual_summary(plan);
        if self.format == Format::Json {
            let mut value = json!(plan);
            value["raw"] = Value::Null;
            value["insights"] = json!(found);
            if let Some(summary) = &summary {
                value["actual"] = json!(summary);
            }
            return self.json(&value);
        }
        let own = culprit_core::self_times(plan);
        match (&summary, &own) {
            (Some(s), Some(_)) => {
                let mut parts = vec![plan_kind(culprit_core::PlanKind::Actual)];
                if let Some(ms) = s.elapsed_ms {
                    parts.push(t!("cli.plan.elapsed", duration = format::duration(ms)));
                }
                if let Some(ms) = s.cpu_ms {
                    parts.push(t!("cli.plan.cpu", duration = format::duration(ms)));
                }
                if let Some(reads) = s.logical_reads {
                    parts.push(t!("cli.plan.reads", n = reads, count = format::count(reads)));
                }
                parts.push(t!("cli.plan.share_of_time"));
                println!("{}", self.paint(DIM, &parts.join(" · ")));
                if let (Some(granted), Some(used)) = (s.memory_granted_kb.filter(|g| *g > 0.0), s.memory_used_kb) {
                    println!("{}", self.paint(DIM, &t!("cli.plan.memory", used = kb(used), granted = kb(granted))));
                }
                if s.spilled {
                    println!("{}", self.paint(YELLOW, &format!("⚠ {}", t!("cli.plan.spilled"))));
                }
            }
            (Some(_), None) => {
                let line = match plan.facts().last_run_at.as_deref() {
                    Some(at) => t!("cli.plan.last_run_at", at = at.replace('T', " ")),
                    None => t!("cli.plan.last_run"),
                };
                println!("{}", self.paint(DIM, &line));
            }
            _ if !plan.weighed() => {
                println!("{}", self.paint(DIM, &t!("cli.plan.access_path", kind = plan_kind(plan.kind))))
            }
            _ => {
                let facts = plan.facts();
                let cost = format::digits(format!("{:.2}", plan.total_cost));
                let line = if facts.virtual_indexes.is_empty() {
                    t!("cli.plan.total_cost", kind = plan_kind(plan.kind), cost = cost)
                } else {
                    t!(
                        "cli.plan.total_cost_virtual",
                        kind = plan_kind(plan.kind),
                        cost = cost,
                        indexes = facts.virtual_indexes.join(", ")
                    )
                };
                println!("{}", self.paint(DIM, &line));
            }
        }
        if let Some(s) = &summary {
            if let Some(granted) = s.memory_granted_kb
                && s.memory_used_kb.is_none()
            {
                println!("{}", t!("cli.plan.memory_granted", granted = kb(granted)));
            }
            if !s.triggers.is_empty() {
                println!("{}", t!("cli.plan.triggers"));
                for trigger in &s.triggers {
                    let time = format::duration(trigger.time_ms);
                    let line = match trigger.calls {
                        Some(c) => t!("cli.plan.trigger_calls", time = time, n = c, count = format::count(c)),
                        None => time,
                    };
                    println!("  {}: {line}", trigger.name);
                }
            }
        }
        let total_time = own.as_ref().map(|t| t.values().sum::<f64>()).filter(|t| *t > 0.0);
        let flagged: Vec<u32> = found.items.iter().filter_map(|i| i.nodes.first().copied()).collect();
        self.plan_node(plan, &plan.root, 0, own.as_ref().zip(total_time), &flagged);
        // Shares of the estimated cost: why a step shows none ("—").
        if total_time.is_none()
            && let Some(note) = culprit_engine::export::shares_note(plan, None)
        {
            println!("{}", self.paint(DIM, &format!("   —  {note}")));
        }
        self.plan_insights(&found, plan);
    }

    /// Insights under the tree: what's wrong, how to fix it, how to check, most important first.
    fn plan_insights(&self, found: &culprit_core::insights::PlanInsights, plan: &Plan) {
        println!();
        if found.items.is_empty() {
            println!("{}", self.paint(BOLD, &t!("cli.plan.no_problems")));
        } else {
            println!("{}", self.paint(BOLD, &t!("cli.plan.insights", count = found.items.len().to_string())));
        }
        for i in &found.items {
            let badge = match i.severity {
                Severity::High => self.paint(RED, &format!("{:<6}", t!("cli.severity.high"))),
                Severity::Medium => self.paint(YELLOW, &format!("{:<6}", t!("cli.severity.medium"))),
                Severity::Low => self.paint(DIM, &format!("{:<6}", t!("cli.severity.low"))),
            };
            let operator = t!("cli.plan.operator", id = i.nodes.first().copied().unwrap_or(0).to_string());
            // A share of the work only where the plan states one (`insights::stated_share`): not a rule's own weight,
            // nor on a plan without costs, a cheap plan or a step whose cost rests on a guess.
            let share = match culprit_core::insights::stated_share(plan, i) {
                Some(share) => format!("{} · ", t!("cli.plan.share_of_work", share = format::percent(share))),
                None => String::new(),
            };
            println!(
                "\n{badge}  {}  {}",
                self.paint(BOLD, &i.title),
                self.paint(DIM, &format!("· {share}{} · {operator}", i.rule))
            );
            println!("        {}", i.detail);
            println!("        {} {}", self.paint(BOLD, &t!("cli.fix")), i.fix);
            if let Some(sql) = &i.fix_sql {
                for line in sql.lines() {
                    println!("            {}", self.paint(DIM, line));
                }
            }
            println!("        {} {}", self.paint(BOLD, &t!("cli.verify")), i.verify);
        }
        if let Some(note) = &found.note {
            println!("\n{}", self.paint(DIM, note));
        }
    }

    fn plan_node(
        &self,
        plan: &Plan,
        node: &PlanNode,
        depth: usize,
        time: Option<(&std::collections::HashMap<u32, f64>, f64)>,
        flagged: &[u32],
    ) {
        let own_ms = time.and_then(|(t, _)| t.get(&node.id).copied());
        // The share of the measured time, else of the estimated cost where the plan can state one (none beside a step it
        // puts no cost on, nor on rows that rest on a default guess).
        let pct = match time {
            Some((t, total)) => Some(t.get(&node.id).copied().unwrap_or(0.0) / total),
            None => plan.step_share(node),
        };
        // A plan without costs or times (SQLite) has no shares or estimates to print.
        let weighed = time.is_some() || plan.weighed();
        let rows = match node.act_rows {
            // The estimate for the runs that happened, so an inner seek estimated right per run isn't shown as off.
            Some(a) => t!(
                "cli.plan.rows_actual",
                estimated = format::count(node.est_rows_for_runs()),
                actual = format::count(a)
            ),
            None if weighed => t!("cli.plan.rows", estimated = format::count(node.est_rows_total())),
            None => String::new(),
        };
        let label = format!(
            "{}{}{}{}",
            if flagged.contains(&node.id) { "◆ " } else { "" },
            node.op,
            node.object.as_deref().map(|o| format!(" {o}")).unwrap_or_default(),
            node.index.as_deref().map(|i| format!(" ({i})")).unwrap_or_default()
        );
        let pct_text = match pct {
            Some(p) if weighed => format!("{:>4}", format::percent(p)),
            None if weighed => "   —".to_string(),
            _ => "   ·".to_string(),
        };
        let pct_text = if weighed && pct.is_some_and(|p| p >= 0.3) { self.paint(RED, &pct_text) } else { pct_text };
        let warnings: Vec<String> = node.warnings.iter().map(|w| w.label()).collect();
        let timing =
            own_ms.filter(|ms| *ms >= 0.5).map(|ms| format!(" · {}", format::duration(ms))).unwrap_or_default();
        println!(
            "{pct_text}  {}{}  {}{}",
            "  ".repeat(depth),
            label,
            self.paint(DIM, &format!("{rows}{timing}")),
            if warnings.is_empty() {
                String::new()
            } else {
                self.paint(YELLOW, &format!("  ⚠ {}", warnings.join(", ")))
            }
        );
        for child in &node.children {
            self.plan_node(plan, child, depth + 1, time, flagged);
        }
    }

    /// A running statement. With `again`, a refresh under `--watch` (text mode reprints compactly).
    pub fn query_progress(&self, progress: &culprit_core::QueryProgress, again: bool) {
        if self.format == Format::Json {
            let mut value = json!(progress);
            if again {
                value["plan"] = Value::Null;
            }
            return self.json(&value);
        }
        if !progress.running {
            println!("{}", self.paint(DIM, &t!("cli.progress.idle", session = &progress.session)));
            return;
        }
        let mut head = vec![t!("cli.progress.session", session = &progress.session)];
        if let Some(ms) = progress.elapsed_ms {
            head.push(t!("cli.progress.running", duration = format::duration(ms)));
        }
        if let Some(ms) = progress.cpu_ms {
            head.push(t!("cli.progress.cpu", duration = format::duration(ms)));
        }
        if let Some(reads) = progress.logical_reads {
            head.push(t!("cli.plan.reads", n = reads, count = format::count(reads)));
        }
        if let Some(rows) = progress.rows_examined {
            head.push(t!("cli.progress.rows_examined", n = rows, count = format::count(rows)));
        }
        if let Some(wait) = &progress.wait {
            head.push(t!("cli.progress.waiting_on", wait = wait));
        }
        println!("{}", self.paint(BOLD, &head.join(" · ")));
        if let Some(phase) = &progress.phase {
            let pct = progress.phase_percent.map(|p| format!(" · {}", format::percent(p))).unwrap_or_default();
            println!("{phase}{pct}");
        }
        if let Some(pct) = progress.percent {
            let basis = progress.percent_basis.as_deref().unwrap_or("");
            println!("{} {}  {}", bar(pct, 24), format::percent(pct), self.paint(DIM, basis));
        }
        let Some(plan) = &progress.plan else {
            if let Some(note) = &progress.note {
                println!("{}", self.paint(DIM, note));
            }
            return;
        };
        let by_id: std::collections::HashMap<u32, &culprit_core::NodeProgress> =
            progress.nodes.iter().map(|n| (n.node_id, n)).collect();
        if let Some(hot) = progress.hot_node.and_then(|id| plan.nodes().into_iter().find(|n| n.id == id)) {
            let label = format!("{}{}", hot.op, hot.object.as_deref().map(|o| format!(" {o}")).unwrap_or_default());
            println!("{}", self.paint(YELLOW, &t!("cli.progress.busiest", step = label)));
        }
        fn visit(
            out: &Out,
            node: &PlanNode,
            depth: usize,
            by_id: &std::collections::HashMap<u32, &culprit_core::NodeProgress>,
        ) {
            let label = format!("{}{}", node.op, node.object.as_deref().map(|o| format!(" {o}")).unwrap_or_default());
            let line = match by_id.get(&node.id) {
                Some(p) => {
                    let state = match p.state {
                        culprit_core::NodeState::Pending => out.paint(DIM, &t!("cli.progress.state.waiting")),
                        culprit_core::NodeState::Running => out.paint(GREEN, &t!("cli.progress.state.running")),
                        culprit_core::NodeState::Done => out.paint(DIM, &t!("cli.progress.state.done")),
                    };
                    let fraction = p.fraction().unwrap_or(0.0);
                    let over = if p.est_rows >= 1.0 && p.rows > p.est_rows * 1.5 {
                        let factor = t!("common.factor", n = format::count(p.rows / p.est_rows));
                        out.paint(YELLOW, &format!(" {}", t!("cli.progress.over_estimate", factor = factor)))
                    } else {
                        String::new()
                    };
                    let rows = t!(
                        "cli.progress.rows",
                        n = p.est_rows,
                        done = format::count(p.rows),
                        estimated = format::count(p.est_rows)
                    );
                    format!(
                        "{} {:>4}  {}{}  {rows}{}  {}",
                        bar(fraction, 10),
                        format::percent(fraction),
                        "  ".repeat(depth),
                        label,
                        over,
                        state
                    )
                }
                None => format!(
                    "{}  {}{}  {}",
                    " ".repeat(16),
                    "  ".repeat(depth),
                    label,
                    t!(
                        "cli.progress.rows_estimated",
                        n = node.est_rows_total(),
                        count = format::count(node.est_rows_total())
                    )
                ),
            };
            println!("{line}");
            for child in &node.children {
                visit(out, child, depth + 1, by_id);
            }
        }
        visit(self, &plan.root, 0, &by_id);
        if let Some(note) = &progress.note {
            println!("{}", self.paint(DIM, note));
        }
    }

    pub fn live(&self, live: &LiveState) {
        if self.format == Format::Json {
            return self.json(&json!(live));
        }
        let blocked: Vec<_> = live.sessions.iter().filter(|s| s.blocked_by.is_some()).collect();
        if blocked.is_empty() {
            if live.blocking_incomplete() {
                println!("{}", t!("cli.live.no_blocker"));
            } else {
                println!("{}", t!("cli.live.no_blocking"));
            }
        } else {
            let n = blocked.len();
            println!("{} {}", self.paint(RED, "!"), t!("cli.live.blocked", n = n, count = n.to_string()));
        }
        for note in &live.notes {
            println!("{}", t!("cli.live.limitation", note = note));
        }
        for s in &live.sessions {
            println!(
                "{:>6}  {:<10} {:>9}  {}{}  {}",
                s.id,
                s.state,
                s.elapsed_ms.map(format::duration).unwrap_or_default(),
                s.wait.as_deref().unwrap_or(""),
                s.blocked_by
                    .as_deref()
                    .map(|b| format!(" {}", t!("cli.live.blocked_by", session = b)))
                    .unwrap_or_default(),
                s.sql
                    .as_deref()
                    .map(|q| q.split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect::<String>()
            );
        }
    }

    pub fn result_set(&self, rs: &ResultSet) {
        if self.format == Format::Json {
            return self.json(&json!(rs));
        }
        println!("{}", rs.columns.join("\t"));
        for row in &rs.rows {
            let cells: Vec<String> = row
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => "NULL".into(),
                    other => other.to_string(),
                })
                .collect();
            println!("{}", cells.join("\t"));
        }
        let tail = if rs.columns.is_empty() {
            rs.messages.join(" ")
        } else {
            let n = rs.rows.len();
            if rs.truncated {
                t!("cli.result.rows_truncated", n = n, count = n.to_string())
            } else {
                t!("cli.result.rows", n = n, count = n.to_string())
            }
        };
        println!("{}", self.paint(DIM, &format!("{tail} · {}", format::duration(rs.elapsed_ms))));
    }

    pub fn catalog(&self, catalog: &culprit_engine::catalog::Catalog) {
        if self.format == Format::Json {
            return self.json(&json!(catalog));
        }
        for t in &catalog.tables {
            let cols: Vec<String> = t.columns.iter().map(|c| format!("{} {}", c.name, c.data_type)).collect();
            println!(
                "{}.{}{}  {}",
                t.schema,
                self.paint(BOLD, &t.name),
                if t.view { format!(" {}", t!("cli.catalog.view")) } else { String::new() },
                self.paint(DIM, &cols.join(", "))
            );
        }
        if !catalog.databases.is_empty() {
            println!("{}", self.paint(DIM, &t!("cli.catalog.databases", databases = catalog.databases.join(", "))));
        }
    }

    pub fn saved_queries(&self, queries: &[culprit_engine::saved::SavedQuery]) {
        if self.format == Format::Json {
            return self.json(&json!({ "queries": queries }));
        }
        if queries.is_empty() {
            println!("{} {}", t!("cli.saved.none"), t!("cli.saved.save_one"));
        }
        for q in queries {
            let first = q.sql.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let target = q.connection.as_deref().map(|c| format!("  ·  {c}")).unwrap_or_default();
            println!("{}{}", self.paint(BOLD, &q.name), self.paint(DIM, &target));
            let first: String = first.chars().take(100).collect();
            println!("  {}", self.paint(DIM, &first));
        }
    }

    pub fn saved_query(&self, query: &culprit_engine::saved::SavedQuery) {
        if self.format == Format::Json {
            return self.json(&json!(query));
        }
        println!("{}", self.paint(BOLD, &query.name));
        if let Some(description) = &query.description {
            println!("{}", self.paint(DIM, description));
        }
        println!("{}", query.sql.trim());
    }

    pub fn index_advice(&self, a: &culprit_core::IndexAdvice) {
        if self.format == Format::Json {
            return self.json(&json!(a));
        }
        let indent =
            |text: &str, prefix: &str| text.lines().map(|l| format!("{prefix}{l}")).collect::<Vec<_>>().join("\n");
        println!(
            "{}  {}",
            self.paint(BOLD, &t!("cli.indexes.title", connection = &a.connection)),
            self.paint(
                DIM,
                &if a.counters.is_empty() {
                    t!("cli.indexes.no_counters")
                } else {
                    t!("cli.indexes.counted", counters = &a.counters)
                },
            )
        );
        if a.candidates.is_empty() && a.cleanups.is_empty() && a.tables.is_empty() {
            println!("\n{}", t!("cli.indexes.nothing"));
        }
        if !a.candidates.is_empty() {
            println!("\n{}", self.paint(BOLD, &t!("cli.indexes.add", count = a.candidates.len().to_string())));
            for (n, c) in a.candidates.iter().enumerate() {
                let weight = match (c.db_time_share, c.queries.len()) {
                    (Some(share), q) if q > 0 => {
                        t!("cli.indexes.share_queries", share = format::percent(share), n = q, count = q.to_string())
                    }
                    (_, q) if q > 0 => t!("cli.indexes.queries", n = q, count = q.to_string()),
                    _ => String::new(),
                };
                let (table, keys) = (format!("{}.{}", c.schema, c.table), c.keys.join(", "));
                let index = if c.include.is_empty() {
                    t!("cli.indexes.index", table = table, keys = keys)
                } else {
                    t!("cli.indexes.index_include", table = table, keys = keys, columns = c.include.join(", "))
                };
                println!("  {:>2}  {}  {}", n + 1, self.paint(BOLD, &index), self.paint(DIM, &weight));
                println!("{}", indent(&c.reason, "      "));
                if let Some(w) = &c.widens {
                    println!("{}", self.paint(DIM, &format!("      {}", t!("cli.indexes.widens", index = w))));
                }
                if c.requests.len() > 1 {
                    let n = c.requests.len();
                    let merges = t!("cli.indexes.merges", n = n, count = n.to_string());
                    println!("{}", self.paint(DIM, &format!("      {merges}")));
                }
                if let Some(sql) = &c.sql {
                    println!("{}", self.paint(BLUE, &indent(sql, "      ")));
                }
                if let Some(cost) = &c.write_cost {
                    println!("{}", self.paint(DIM, &format!("      {cost}")));
                }
            }
        }
        if !a.cleanups.is_empty() {
            println!("\n{}", self.paint(BOLD, &t!("cli.indexes.clean_up", count = a.cleanups.len().to_string())));
            for c in &a.cleanups {
                let kind = match c.kind {
                    culprit_core::CleanupKind::Duplicate => t!("cli.indexes.kind.duplicate"),
                    culprit_core::CleanupKind::Prefix => t!("cli.indexes.kind.covered"),
                    culprit_core::CleanupKind::Overlap => t!("cli.indexes.kind.overlaps"),
                    culprit_core::CleanupKind::Unused => t!("cli.indexes.kind.unused"),
                };
                println!(
                    "  {}  {}  {}",
                    self.paint(YELLOW, &format!("{kind:<9}")),
                    self.paint(BOLD, &format!("{}.{}.{}", c.schema, c.table, c.index)),
                    self.paint(DIM, &c.size_bytes.map(format::bytes).unwrap_or_default())
                );
                println!("{}", indent(&c.reason, "             "));
                if let Some(sql) = &c.sql {
                    println!("{}", self.paint(BLUE, &indent(sql, "             ")));
                }
            }
        }
        if !a.tables.is_empty() {
            println!("\n{}", self.paint(BOLD, &t!("cli.indexes.tables", count = a.tables.len().to_string())));
            for t in &a.tables {
                println!("  {}  {}", self.paint(BOLD, &format!("{}.{}", t.schema, t.table)), t.reason);
            }
        }
        for note in &a.notes {
            println!("\n{}", self.paint(DIM, note));
        }
    }
}
