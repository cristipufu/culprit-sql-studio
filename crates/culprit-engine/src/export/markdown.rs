//! Markdown exports, written for a pull request description or an issue: a short lead, the evidence, the fix, how to
//! verify it. Renders well on GitHub, GitLab and in chat tools.

use std::fmt::Write;

use culprit_core::{
    ChangeKind, Finding, Metric, Plan, PlanDiff, PlanNode, Report, Verification, VerifyStatus, format, t,
};

use super::{
    ExportNote, ExportOptions, estimated_cost, exported_at, finding_line, kind_inline, plan_engine_label, prepared,
    rows_phrase, rows_text, server_line, share, shares_note, sql_text, stats_line, top_costs, where_,
};

const MAX_STEPS: usize = 40;
const NBSP: char = '\u{a0}';

/// A table cell: kept on one line, with HTML, Markdown and pipes escaped.
fn cell(raw: &str) -> String {
    text(raw)
}

/// Database-sourced text (names, titles built from them) outside code: HTML and Markdown that would render in a pull
/// request or wiki become plain characters. A table named `<img src=x onerror=…>` stays text.
fn text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            // Characters Markdown would turn into links, images, emphasis or headings inside a line.
            '[' | ']' | '*' | '_' | '`' | '|' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// Inline code that survives backticks in the text.
fn code(text: &str) -> String {
    if text.contains('`') { format!("`` {text} ``") } else { format!("`{text}`") }
}

fn fence(out: &mut String, lang: &str, text: &str) {
    let ticks = if text.contains("```") { "````" } else { "```" };
    let _ = writeln!(out, "{ticks}{lang}\n{}\n{ticks}\n", text.trim_end());
}

fn redaction_note(out: &mut String, options: &ExportOptions) {
    if options.redact {
        let _ = writeln!(out, "> {}\n", t!("engine.export.redacted"));
    }
}

/// When it was exported, and the finding it is when it is one.
fn footer(out: &mut String, finding: Option<&str>) {
    let footer = match finding {
        Some(id) => t!("engine.export.md.footer_finding", when = exported_at(), id = code(id)),
        None => t!("engine.export.md.footer", when = exported_at()),
    };
    let _ = writeln!(out, "<sub>{footer}</sub>");
}

/// The table's column of rows: estimated, or estimated and actual when the plan ran.
pub(super) fn rows_column(plan: &Plan) -> String {
    if plan.kind == culprit_core::PlanKind::Actual {
        t!("engine.export.column.rows_actual")
    } else {
        t!("engine.export.column.rows_estimated")
    }
}

/// One plan: where the cost goes, the operators as an indented table, warnings, notes and the statement.
pub fn plan_markdown(plan: &Plan, title: &str, options: &ExportOptions) -> String {
    let plan = prepared(plan, options);
    let mut out = String::new();
    let count = plan.nodes().len();
    let _ = writeln!(out, "## {}\n", text(title.trim()));
    let line = t!(
        "engine.export.plan_line",
        engine = plan_engine_label(&plan),
        kind = kind_inline(plan.kind),
        n = count,
        count = count.to_string(),
        cost = estimated_cost(&plan)
    );
    let _ = writeln!(out, "{line}\n");
    redaction_note(&mut out, options);
    plan_body(&mut out, &plan, &options.notes);
    footer(&mut out, None);
    out
}

fn plan_body(out: &mut String, plan: &Plan, notes: &[ExportNote]) {
    let costs: Vec<String> =
        top_costs(plan, 4).iter().map(|(n, s)| format!("**{}** {}", format::percent(*s), cell(&label(n)))).collect();
    if !costs.is_empty() {
        let _ = writeln!(out, "**{}** {}\n", t!("engine.export.md.where_cost_goes"), costs.join(" · "));
    }
    let hot = plan.hottest().map(|n| n.id);
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} |",
        t!("engine.export.column.step"),
        t!("engine.export.column.table_index"),
        t!("engine.export.column.cost"),
        rows_column(plan),
        t!("engine.export.column.notes")
    );
    let _ = writeln!(out, "|---|---|---:|---:|---|");
    let mut rows = Vec::new();
    walk(&plan.root, 0, &mut rows);
    for (node, depth) in rows.iter().take(MAX_STEPS) {
        let indent: String = std::iter::repeat_n(NBSP, depth * 3).collect();
        let arrow = if *depth > 0 { "↳ " } else { "" };
        let mut notes_cell: Vec<String> = node.warnings.iter().map(|w| w.label().to_string()).collect();
        if Some(node.id) == hot && share(plan, node).is_some_and(|s| s >= 0.05) {
            notes_cell.insert(0, t!("engine.export.most_expensive"));
        }
        // An exchange's cost is the charge for passing rows between threads: never the step where the work is.
        if node.kind == culprit_core::NodeKind::Parallelism && share(plan, node).is_some_and(|s| s >= 0.05) {
            notes_cell.insert(0, t!("engine.export.passing_rows"));
        }
        let _ = writeln!(
            out,
            "| {indent}{arrow}{} | {} | {} | {} | {} |",
            cell(&node.op),
            cell(&where_(node)),
            // No share where the plan can't state one (no costs, a step it doesn't cost, a default guess).
            share(plan, node).map(format::percent).unwrap_or_else(|| "—".into()),
            rows_text(node),
            cell(&notes_cell.join(", "))
        );
    }
    if rows.len() > MAX_STEPS {
        let more = rows.len() - MAX_STEPS;
        let _ = writeln!(out, "\n_{}_", t!("engine.export.md.more_operators", n = more, count = more.to_string()));
    }
    let _ = writeln!(out);
    if let Some(note) = shares_note(plan, None) {
        let _ = writeln!(out, "_{}_\n", text(&note.replace('`', "")));
    }
    let predicates: Vec<String> = rows
        .iter()
        .filter(|(n, _)| share(plan, n).is_some_and(|s| s >= 0.05))
        .flat_map(|(n, _)| {
            let mut lines = Vec::new();
            if let Some(p) = &n.seek_predicate {
                let line =
                    t!("engine.export.seeks_on", step = cell(&label(n)), predicate = code(&format::predicate(p)));
                lines.push(format!("- {line}"));
            }
            if let Some(p) = &n.predicate {
                let line = t!("engine.export.filters", step = cell(&label(n)), predicate = code(&format::predicate(p)));
                lines.push(format!("- {line}"));
            }
            lines
        })
        .collect();
    if !predicates.is_empty() {
        let _ = writeln!(out, "**{}**\n\n{}\n", t!("engine.export.predicates_heading"), predicates.join("\n"));
    }
    if !plan.warnings.is_empty() {
        let names: Vec<String> = plan.warnings.iter().map(|w| w.label()).collect();
        let _ = writeln!(out, "**{}** {}\n", t!("engine.export.md.plan_warnings"), names.join(", "));
    }
    if !plan.index_hints.is_empty() {
        let _ = writeln!(out, "**{}**\n", t!("engine.export.index_hints_heading"));
        for h in &plan.index_hints {
            let keys: Vec<String> = h.equality.iter().chain(h.inequality.iter()).cloned().collect();
            let (table, keys, percent) = (code(&h.table), keys.join(", "), format::percent(h.impact_pct / 100.0));
            let hint = if h.include.is_empty() {
                t!("engine.export.index_hint", table = table, keys = keys, percent = percent)
            } else {
                let columns = h.include.join(", ");
                t!("engine.export.index_hint_include", table = table, keys = keys, columns = columns, percent = percent)
            };
            let _ = writeln!(out, "- {hint}");
        }
        let _ = writeln!(out);
    }
    notes_section(out, notes, plan);
    if let Some(statement) = &plan.statement {
        let _ = writeln!(out, "<details><summary>{}</summary>\n", t!("engine.export.statement"));
        fence(out, "sql", statement);
        let _ = writeln!(out, "</details>\n");
    }
}

fn notes_section(out: &mut String, notes: &[ExportNote], plan: &Plan) {
    if notes.is_empty() {
        return;
    }
    let _ = writeln!(out, "**{}**\n", t!("engine.export.insights"));
    for n in notes {
        let on = n
            .node
            .and_then(|id| plan.nodes().into_iter().find(|x| x.id == id))
            .map(|x| format!(" ({})", label(x)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "- **{}**{on}{}",
            cell(&n.title),
            n.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        );
        if let Some(fix) = &n.fix {
            let _ = writeln!(out, "  {}", t!("engine.export.note_fix", fix = fix));
        }
    }
    let _ = writeln!(out);
}

fn walk<'a>(node: &'a PlanNode, depth: usize, out: &mut Vec<(&'a PlanNode, usize)>) {
    out.push((node, depth));
    for c in &node.children {
        walk(c, depth + 1, out);
    }
}

fn label(node: &PlanNode) -> String {
    super::node_label(node)
}

/// Two plans compared: the lead sentence, the numbers side by side, what changed, why the optimizer may have chosen
/// differently, and each plan's steps.
pub fn diff_markdown(
    a: &Plan,
    b: &Plan,
    diff: &PlanDiff,
    name_a: &str,
    name_b: &str,
    options: &ExportOptions,
) -> String {
    let (a, b) = (prepared(a, options), prepared(b, options));
    let redacted_diff = options.redact.then(|| culprit_core::plan_diff_named(&a, name_a, &b, name_b));
    let diff = redacted_diff.as_ref().unwrap_or(diff);
    let mut out = String::new();
    let _ = writeln!(out, "## {}\n", t!("engine.export.plan_comparison", a = name_a, b = name_b));
    redaction_note(&mut out, options);
    if let Some((lead, rest)) = diff.headlines.split_first() {
        let _ = writeln!(out, "**{}**\n", lead);
        for h in rest {
            let _ = writeln!(out, "- {h}");
        }
        if !rest.is_empty() {
            let _ = writeln!(out);
        }
    }
    let _ = writeln!(out, "| | {} | {} |\n|---|---:|---:|", cell(name_a), cell(name_b));
    let row = |label: String, a: String, b: String| format!("| {label} | {a} | {b} |");
    let _ = writeln!(
        out,
        "{}",
        row(t!("engine.export.row.engine"), plan_engine_label(&a).into(), plan_engine_label(&b).into())
    );
    let _ = writeln!(out, "{}", row(t!("engine.export.plan"), kind_inline(a.kind), kind_inline(b.kind)));
    let _ = writeln!(out, "{}", row(t!("engine.export.row.estimated_cost"), estimated_cost(&a), estimated_cost(&b)));
    if let (Some(ta), Some(tb)) = (diff.time_a_ms, diff.time_b_ms) {
        let _ =
            writeln!(out, "{}", row(t!("engine.export.row.actual_time"), format::duration(ta), format::duration(tb)));
    }
    let operators = row(t!("engine.export.row.operators"), a.nodes().len().to_string(), b.nodes().len().to_string());
    let _ = writeln!(out, "{operators}\n");
    if !diff.changes.is_empty() {
        let _ = writeln!(out, "### {}\n", t!("engine.export.what_changed"));
        for c in diff.changes.iter().take(20) {
            let detail = c.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
            let _ = writeln!(out, "- **{}** {}{}", change_word(c.kind), text(&c.title), text(&detail));
        }
        let _ = writeln!(out);
    }
    if !diff.scenarios.is_empty() {
        let _ = writeln!(out, "### {}\n", t!("engine.export.why_differently"));
        for s in &diff.scenarios {
            let detail = s.detail.as_deref().map(|d| format!(" {d}")).unwrap_or_default();
            let _ = writeln!(out, "- **{}**{}", text(&s.title), text(&detail));
        }
        let _ = writeln!(out);
    }
    for (name, plan) in [(name_a, &a), (name_b, &b)] {
        let count = plan.nodes().len();
        let summary = t!("engine.export.md.plan_operators", name = text(name), n = count, count = count.to_string());
        let _ = writeln!(out, "<details><summary>{summary}</summary>\n");
        plan_body(&mut out, plan, &[]);
        let _ = writeln!(out, "</details>\n");
    }
    notes_section(&mut out, &options.notes, &b);
    footer(&mut out, None);
    out
}

/// What kind of change a comparison found: "Access", "Cost".
pub(super) fn change_word(kind: ChangeKind) -> String {
    match kind {
        ChangeKind::Access => t!("engine.export.change.access"),
        ChangeKind::Operator => t!("engine.export.change.operator"),
        ChangeKind::Added => t!("engine.export.change.added"),
        ChangeKind::Removed => t!("engine.export.change.removed"),
        ChangeKind::Cost => t!("engine.export.change.cost"),
        ChangeKind::Rows => t!("engine.export.change.rows"),
        ChangeKind::Warning => t!("engine.export.change.warning"),
    }
}

/// A finding's metric as the brief says it (`culprit_checks::metric_text`): a total wait to the second, as its title.
pub(crate) fn metric_text(m: &Metric) -> String {
    culprit_checks::metric_text(m)
}

/// One finding in full for a digest (a chat thread, a ticket): the same pieces as [`finding_markdown`], always
/// redacted, as data for the consumer to lay out.
pub fn finding_detail(report: &Report, finding: &Finding) -> culprit_core::DigestDetail {
    let shared = super::shared_report(report);
    detail(report, super::shareable(report, &shared, finding))
}

/// Digest assembly already holds a report checked from redacted input; do not repeat analysis per finding.
pub(crate) fn finding_detail_shared(report: &Report, finding: &Finding) -> culprit_core::DigestDetail {
    detail(report, super::shareable(report, report, finding))
}

fn detail(report: &Report, f: Finding) -> culprit_core::DigestDetail {
    let s = &report.snapshot;
    let engine = s.server.engine;
    let queries = s
        .workload
        .as_ref()
        .map(|w| {
            f.queries
                .iter()
                .filter_map(|r| w.query(&r.id).map(|q| (r, q)))
                .take(2)
                .map(|(r, q)| {
                    let plan = q.plan.as_ref().and_then(|plan| {
                        let plan = culprit_core::redact::redact_plan(plan);
                        // The step the finding names, else its plan's working step where the finding is about one.
                        let node = culprit_checks::finding_step(&f, r, &plan)?;
                        // A plan without costs (SQLite) names the step, and no share or estimate it doesn't have; a
                        // share only where the plan can state one.
                        let mut line = match (plan.weighed(), culprit_checks::finding_step_share(&plan, node)) {
                            (true, Some(s)) => format!(
                                "{}: {} of the plan's cost, {}",
                                label(node),
                                format::percent(s),
                                rows_phrase(node)
                            ),
                            (true, None) => format!("{}: {}", label(node), rows_phrase(node)),
                            (false, _) => label(node),
                        };
                        if let Some(p) = node.predicate.as_deref().or(node.seek_predicate.as_deref()) {
                            let _ = write!(line, ", on `{}`", format::predicate(p));
                        }
                        Some(line)
                    });
                    culprit_core::DigestQuery {
                        stats: stats_line(w, q),
                        sql: culprit_core::redact::redact_sql(q.text.trim(), engine),
                        plan,
                        note: r.note.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    culprit_core::DigestDetail {
        confidence: format!("{:?}", f.confidence).to_lowercase(),
        summary: f.summary,
        metrics: f
            .metrics
            .iter()
            .take(6)
            .map(|m| culprit_core::DigestMetric { label: m.label.clone(), value: metric_text(m) })
            .collect(),
        queries,
        fix: f.fix,
        verify: f.verify,
        caveats: f.caveats,
    }
}

/// One finding as a PR section: what's happening, the evidence, the query and its plan, the fix and how to verify it.
pub fn finding_markdown(report: &Report, finding: &Finding, options: &ExportOptions) -> String {
    let mut out = String::new();
    let shared = options.redact.then(|| super::shared_report(report));
    finding_section(&mut out, report, shared.as_ref(), finding, options, "##");
    footer(&mut out, Some(&finding.id));
    out
}

/// `shared` is the report checked from its redacted snapshot when the export redacts (see `shared_report`).
fn finding_section(
    out: &mut String,
    report: &Report,
    shared: Option<&Report>,
    finding: &Finding,
    options: &ExportOptions,
    level: &str,
) {
    let s = &report.snapshot;
    let redacted;
    let finding = match shared {
        Some(shared) => {
            redacted = super::shareable(report, shared, finding);
            &redacted
        }
        None => finding,
    };
    let _ = writeln!(out, "{level} {}\n", text(&finding.title));
    let impact = if finding.impact.label.is_empty() { String::new() } else { format!("**{}**", finding.impact.label) };
    let _ = writeln!(out, "{}\n", finding_line(report, finding, impact));
    let _ = writeln!(out, "{}\n", text(&finding.summary));
    if !finding.metrics.is_empty() {
        for m in &finding.metrics {
            let _ = writeln!(out, "- {}: **{}**", text(&m.label), text(&metric_text(m)));
        }
        let _ = writeln!(out);
    }
    let engine = s.server.engine;
    let queries: Vec<_> =
        finding.queries.iter().filter_map(|r| s.workload.as_ref()?.query(&r.id).map(|q| (r, q))).take(3).collect();
    for (r, q) in &queries {
        let w = s.workload.as_ref().expect("filtered on workload");
        let _ = writeln!(out, "**{}** · {}\n", t!("engine.export.query", id = code(&q.id)), stats_line(w, q));
        fence(out, "sql", &sql_text(&q.text, engine, options));
        if let Some(plan) = &q.plan {
            let plan = prepared(plan, options);
            // The step the finding names, else its plan's working step where the finding is about one.
            if let Some(node) = culprit_checks::finding_step(finding, r, &plan) {
                let step = format!("**{}**", label(node));
                let predicate =
                    node.predicate.as_deref().or(node.seek_predicate.as_deref()).map(|p| code(&format::predicate(p)));
                let share = plan.weighed().then(|| culprit_checks::finding_step_share(&plan, node));
                let mut line = match (share, predicate) {
                    (None, None) => t!("engine.export.md.step.plain", step = step),
                    (None, Some(predicate)) => t!("engine.export.md.step.on", step = step, predicate = predicate),
                    (Some(None), None) => t!("engine.export.md.step.rows", step = step, rows = rows_phrase(node)),
                    (Some(None), Some(predicate)) => t!(
                        "engine.export.md.step.rows_on",
                        step = step,
                        rows = rows_phrase(node),
                        predicate = predicate
                    ),
                    (Some(Some(s)), None) => t!(
                        "engine.export.md.step.share_rows",
                        step = step,
                        share = format::percent(s),
                        rows = rows_phrase(node)
                    ),
                    (Some(Some(s)), Some(predicate)) => t!(
                        "engine.export.md.step.share_rows_on",
                        step = step,
                        share = format::percent(s),
                        rows = rows_phrase(node),
                        predicate = predicate
                    ),
                };
                // Where the plan can't state the step's share, why.
                if let Some(why) = culprit_checks::finding_no_share_reason(&plan, node, Some(q)) {
                    let _ = write!(line, " {}", text(&why.replace('`', "")));
                }
                let _ = writeln!(out, "{line}\n");
            }
        }
    }
    if let Some(fix) = &finding.fix {
        let _ = writeln!(out, "**{}** {}\n", t!("engine.export.md.fix"), text(&fix.summary));
        if let Some(sql) = &fix.sql {
            fence(out, "sql", sql);
        }
        if let Some(cost) = &fix.cost {
            let _ = writeln!(out, "_{}_ {}\n", t!("engine.export.md.cost"), text(cost));
        }
    }
    if !finding.verify.is_empty() {
        let _ = writeln!(out, "**{}**\n", t!("engine.export.how_to_verify"));
        for (i, step) in finding.verify.iter().enumerate() {
            let _ = writeln!(out, "{}. {step}", i + 1);
        }
        // Only where a re-scan can show it gone: not for statements that stopped, nor on running totals.
        if culprit_checks::gone_after_rescan(shared.unwrap_or(report), finding) {
            let command = format!("`culprit verify <connection> {}`", finding.id);
            let _ = writeln!(
                out,
                "{}. {}",
                finding.verify.len() + 1,
                t!("engine.export.md.verify_after", command = command)
            );
        }
        out.push('\n');
    }
    if !finding.caveats.is_empty() {
        let _ = writeln!(out, "**{}**\n", t!("engine.export.before_you_act"));
        for c in &finding.caveats {
            let _ = writeln!(out, "- {c}");
        }
        let _ = writeln!(out);
    }
    if let Some(plan) = queries.first().and_then(|(_, q)| q.plan.as_ref()) {
        notes_section(out, &options.notes, plan);
    }
}

/// Every finding of a scan: a summary table and the top findings in full.
pub fn report_markdown(report: &Report, options: &ExportOptions) -> String {
    let mut out = String::new();
    let s = &report.snapshot;
    let n = report.findings.len();
    let _ = writeln!(out, "## {}\n", t!("engine.export.md.report_title", n = n, count = n.to_string()));
    let _ =
        writeln!(out, "{}\n", t!("engine.export.md.report_scanned", server = server_line(report), when = &s.taken_at));
    redaction_note(&mut out, options);
    let shared = options.redact.then(|| super::shared_report(report));
    if report.findings.is_empty() {
        let _ = writeln!(out, "{}\n", t!("engine.export.nothing_stands_out"));
    } else {
        let (finding, impact) = (t!("engine.export.column.finding"), t!("engine.export.column.impact"));
        let _ = writeln!(out, "| | {finding} | {impact} |\n|---|---|---|");
        for f in &report.findings {
            let title = match &shared {
                Some(shared) => super::shareable(report, shared, f).title,
                None => f.title.clone(),
            };
            let _ = writeln!(out, "| {} | {} | {} |", severity_dot(f), cell(&title), cell(&f.impact.label));
        }
        let _ = writeln!(out);
        for f in report.findings.iter().take(5) {
            finding_section(&mut out, report, shared.as_ref(), f, options, "###");
        }
        if report.findings.len() > 5 {
            let other = report.findings.len() - 5;
            let line = t!(
                "engine.export.md.other_findings",
                n = other,
                count = other.to_string(),
                command = "`culprit show <connection> <id>`"
            );
            let _ = writeln!(out, "_{line}_\n");
        }
    }
    footer(&mut out, None);
    out
}

fn severity_dot(f: &Finding) -> &'static str {
    if !f.is_actionable() {
        return "○";
    }
    match f.severity {
        culprit_core::Severity::High => "🔴",
        culprit_core::Severity::Medium => "🟠",
        culprit_core::Severity::Low => "🔵",
    }
}

/// The verification as Markdown, for a PR comment after the fix shipped.
pub fn verification_markdown(v: &Verification) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "**{}**\n", text(&v.summary));
    for f in v.findings.iter().chain(v.new_findings.iter()) {
        let mark = match f.status {
            VerifyStatus::Fixed => format!("✅ {}", t!("engine.export.verify.fixed")),
            VerifyStatus::StillPresent => format!("⏳ {}", t!("engine.export.verify.still_present")),
            VerifyStatus::Unknown => format!("❔ {}", t!("engine.export.verify.unknown")),
            VerifyStatus::New => format!("🆕 {}", t!("engine.export.verify.new")),
        };
        let impact = match (&f.before, &f.after) {
            (Some(b), Some(a)) if b != a => format!(" ({b} → {a})"),
            (Some(b), None) => format!(" ({})", t!("engine.export.verify.was", before = b)),
            (_, Some(a)) => format!(" ({a})"),
            _ => String::new(),
        };
        let _ = writeln!(out, "- {mark}: {}{}", text(&f.title), text(&impact));
        if let Some(note) = &f.note {
            let _ = writeln!(out, "  {}", text(note));
        }
    }
    let (before, after) = culprit_core::changes::range_times(&v.before_taken_at, &v.after_taken_at);
    let _ = writeln!(out, "\n<sub>{}</sub>", t!("engine.export.md.scans", before = before, after = after));
    out
}

#[cfg(test)]
mod tests {
    use super::{cell, text};

    #[test]
    fn database_text_stays_text() {
        assert_eq!(text("T <img src=x onerror=alert(1)>"), "T &lt;img src=x onerror=alert(1)&gt;");
        assert_eq!(text("[click](https://example.com)"), "\\[click\\](https://example.com)");
        assert_eq!(text("order_items is *hot*"), "order\\_items is \\*hot\\*");
        assert_eq!(cell("a | b\nc"), "a \\| b c");
    }
}
