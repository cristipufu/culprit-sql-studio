//! Self-contained HTML exports: one file that opens anywhere, offline, with no scripts, fonts or network requests.
//! Everything from the database is escaped. Light and dark follow the reader's system, and pages print cleanly.

use std::collections::HashMap;
use std::fmt::Write;

use culprit_core::{ChangeKind, Finding, Plan, PlanDiff, PlanNode, Report, ScenarioKind, Severity, Unit, format, t};

use super::markdown::{change_word, metric_text, rows_column};
use super::svg::{Mark, diagram, escape, heat_class};
use super::{
    ExportNote, ExportOptions, estimated_cost, exported_at, finding_line, kind_inline, kind_label, node_label,
    plan_engine_label, prepared, report_engine_label, rows_phrase, rows_text, server_line, share, shares_note,
    sql_text, stats_line, top_costs, where_,
};

const STYLE: &str = r#"
:root{color-scheme:light dark;--sans:-apple-system,BlinkMacSystemFont,"Segoe UI Variable Text","Segoe UI",Inter,system-ui,sans-serif;--mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,Consolas,monospace;
--bg:#fafaf9;--surface:#fff;--sunken:#f4f4f2;--border:#e8e7e3;--border-strong:#d6d4ce;--text:#1b1a18;--text-2:#55534e;--text-3:#77746e;
--accent:#3b6fe8;--brand:#ffcf33;--good:#23824f;--good-bg:#e7f5ee;--bad:#c4363c;--bad-bg:#fdeceb;--warn:#8f5400;--warn-bg:#fdf3e1;--code-bg:#f4f4f1;
--h0:#e8e7e3;--h1:#f7d77a;--h2:#f5b94f;--h3:#ef8b3a;--h4:#e2553f;
--k-scan:#b56b00;--k-seek:#23824f;--k-lookup:#c4621c;--k-join:#3b6fe8;--k-sort:#8338b5;--k-agg:#13808a;--k-write:#c4363c;--k-dim:#8d8a84;
--t0:#e9f0fe;--t1:#e4f5ec;--t2:#f2eafb;--t3:#fff3d6;--t4:#e0f3f5;--t5:#fdecef;--changed:#d18700;--only:#9a978f}
@media (prefers-color-scheme:dark){:root{--bg:#131312;--surface:#1b1b19;--sunken:#222220;--border:#2d2c29;--border-strong:#3c3b37;--text:#ecebe6;--text-2:#b6b4ad;--text-3:#8f8d86;
--accent:#7aa2ff;--good:#4cc287;--good-bg:#16301f;--bad:#f07076;--bad-bg:#3a1c1e;--warn:#e6a94a;--warn-bg:#332812;--code-bg:#222220;
--h0:#33322e;--h1:#6b5a22;--h2:#8a6420;--h3:#a3561f;--h4:#c2452f;
--k-scan:#e3a03a;--k-seek:#4cc287;--k-lookup:#e98a4b;--k-join:#7aa2ff;--k-sort:#b98ae0;--k-agg:#4cc0c8;--k-write:#f07076;--k-dim:#8f8d86;
--t0:#1c2945;--t1:#173224;--t2:#2a2140;--t3:#37301a;--t4:#163236;--t5:#3a1e25;--changed:#e6a94a;--only:#77746e}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:13px/1.55 var(--sans);-webkit-font-smoothing:antialiased}
main{max-width:1120px;margin:0 auto;padding:36px 28px 56px}
.top{display:flex;align-items:center;gap:10px;margin-bottom:28px;color:var(--text-3);font-size:12px}
.mark{width:22px;height:22px;border-radius:6px;background:var(--brand);color:#1b1a18;display:grid;place-items:center;font:800 13px/1 var(--sans)}
.brand{font-weight:650;color:var(--text);font-size:13px}
.spacer{flex:1}
.chip{display:inline-flex;align-items:center;gap:5px;padding:2px 8px;border-radius:999px;border:1px solid var(--border);background:var(--surface);color:var(--text-2);font-size:12px;white-space:nowrap}
.chip.redacted{border-color:transparent;background:var(--sunken)}
h1{font-size:22px;line-height:1.3;font-weight:650;letter-spacing:-.01em;margin:0 0 6px}
.sub{color:var(--text-2);font-size:13px;margin:0 0 4px}
h2{font-size:11px;font-weight:650;letter-spacing:.07em;text-transform:uppercase;color:var(--text-3);margin:34px 0 10px}
p{margin:0 0 10px}
.lead{font-size:17px;line-height:1.45;font-weight:600;margin:4px 0 10px;max-width:880px}
.more{margin:0;padding:0;list-style:none;color:var(--text-2);max-width:880px}
.more li{padding:3px 0 3px 16px;position:relative}
.more li:before{content:"";position:absolute;left:3px;top:11px;width:5px;height:5px;border-radius:50%;background:var(--border-strong)}
.verdict{display:inline-block;padding:2px 9px;border-radius:999px;font-size:12px;font-weight:650;margin-right:8px;vertical-align:3px}
.verdict.worse{background:var(--bad-bg);color:var(--bad)}.verdict.better{background:var(--good-bg);color:var(--good)}.verdict.same{background:var(--sunken);color:var(--text-2)}
.figures{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));border:1px solid var(--border);border-radius:12px;overflow:hidden;background:var(--surface);margin-top:18px}
.fig{padding:12px 14px;box-shadow:1px 0 0 var(--border),0 1px 0 var(--border)}
.fig .v{font-size:20px;font-weight:650;font-variant-numeric:tabular-nums;letter-spacing:-.01em}
.fig .l{font-size:12px;color:var(--text-3)}
.strip{display:flex;height:10px;border-radius:5px;overflow:hidden;background:var(--sunken);gap:2px}
.strip span{display:block;min-width:3px}
.legend{display:flex;flex-wrap:wrap;gap:6px 16px;margin:8px 0 0;padding:0;list-style:none;font-size:12px;color:var(--text-2)}
.legend b{font-variant-numeric:tabular-nums;color:var(--text)}
.sw{display:inline-block;width:9px;height:9px;border-radius:3px;margin-right:6px;vertical-align:0}
.h0{background:var(--h0)}.h1{background:var(--h1)}.h2{background:var(--h2)}.h3{background:var(--h3)}.h4{background:var(--h4)}
.wrap{border:1px solid var(--border);border-radius:12px;background:var(--surface) radial-gradient(circle,var(--border) 1px,transparent 1.2px) 0 0/18px 18px;overflow:auto;padding:8px}
.wrap svg{display:block;margin:0 auto;max-width:none}
.side{margin-top:22px}
.side-head{display:flex;align-items:baseline;gap:8px;margin:0 0 8px}
.badge{display:inline-grid;place-items:center;min-width:20px;height:20px;padding:0 5px;border-radius:6px;background:var(--text);color:var(--bg);font-size:11px;font-weight:700}
.side-head b{font-size:14px}
.side-head span{color:var(--text-3);font-size:12px}
.keys{display:flex;flex-wrap:wrap;gap:6px 18px;font-size:12px;color:var(--text-2);margin:10px 0 0}
.key{display:inline-flex;align-items:center;gap:6px}
.key i{display:inline-block;width:16px;height:11px;border-radius:3px;border:1px solid var(--border-strong)}
.key i.tint{background:var(--t0)}.key i.changed{border:1.5px dashed var(--changed)}.key i.only{border:1.5px dashed var(--only);background:var(--sunken)}
ul.changes,ul.scen{list-style:none;margin:0;padding:0;border:1px solid var(--border);border-radius:12px;background:var(--surface);overflow:hidden}
.changes li,.scen li{display:grid;grid-template-columns:86px 1fr;gap:12px;padding:9px 14px;border-top:1px solid var(--border)}
.changes li:first-child,.scen li:first-child{border-top:0}
.tag{display:inline-block;padding:1px 7px;border-radius:5px;font-size:11px;font-weight:650;letter-spacing:.02em;background:var(--sunken);color:var(--text-2);text-align:center;align-self:start;margin-top:1px}
.tag.access,.tag.operator{background:var(--warn-bg);color:var(--warn)}.tag.added{background:var(--good-bg);color:var(--good)}.tag.removed{background:var(--bad-bg);color:var(--bad)}
.tag.cost,.tag.rows{background:var(--t0);color:var(--accent)}
.changes .t,.scen .t{font-weight:600}
.changes .d,.scen .d{color:var(--text-2);font-size:12.5px}
table.steps{width:100%;border-collapse:separate;border-spacing:0;border:1px solid var(--border);border-radius:12px;overflow:hidden;background:var(--surface);font-size:12.5px}
.steps th{text-align:left;font-size:11px;font-weight:650;letter-spacing:.05em;text-transform:uppercase;color:var(--text-3);padding:8px 12px;background:var(--sunken);border-bottom:1px solid var(--border)}
.steps td{padding:6px 12px;border-top:1px solid var(--border);vertical-align:top}
.steps tr:first-child td{border-top:0}
.steps .r{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.steps .obj{font-family:var(--mono);font-size:12px;color:var(--text-2)}
.steps .op{font-weight:600}
.steps .hotrow td{background:color-mix(in srgb,var(--h4) 8%,transparent)}
.pctbar{display:inline-block;width:42px;height:4px;border-radius:2px;background:var(--sunken);margin-right:8px;vertical-align:2px;overflow:hidden}
.pctbar i{display:block;height:100%}
.warnings{color:var(--warn);font-size:12px}
pre{margin:0;padding:12px 14px;border-radius:10px;background:var(--code-bg);border:1px solid var(--border);font:12.5px/1.6 var(--mono);white-space:pre-wrap;word-break:break-word}
code{font-family:var(--mono);font-size:12px;background:var(--code-bg);padding:1px 5px;border-radius:4px}
.metrics{display:flex;flex-wrap:wrap;border:1px solid var(--border);border-radius:12px;overflow:hidden;background:var(--surface)}
.metrics>.fig{flex:1 1 170px;min-width:0;overflow-wrap:anywhere}
.metrics>.metric-text{flex-basis:100%}
.metric-text .l{font-weight:600;margin-bottom:4px}
.metric-text .v{font-size:13px;font-weight:400;line-height:1.6;letter-spacing:normal;white-space:pre-wrap}
.callout{border:1px solid var(--border);border-left:3px solid var(--accent);border-radius:10px;padding:12px 14px;background:var(--surface)}
ol.verify{margin:0;padding-left:20px}ol.verify li{padding:2px 0}
.sev{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:7px}
.sev.high{background:var(--bad)}.sev.medium{background:#e08a0b}.sev.low{background:#6f8bb8}
.qmeta{color:var(--text-2);font-size:12.5px;margin:0 0 8px}
.note{color:var(--text-2)}
.finding+.finding{margin-top:44px;padding-top:30px;border-top:1px solid var(--border)}
footer{margin-top:44px;color:var(--text-3);font-size:12px;display:flex;gap:8px;flex-wrap:wrap}
svg.diagram .box{fill:var(--surface);stroke:var(--border-strong);stroke-width:1}
svg.diagram .card.hot .box{stroke:var(--h4);stroke-opacity:.7}
svg.diagram .card.focus .box{stroke:var(--accent);stroke-width:2.2}
svg.diagram .card.tint.t0 .box{fill:var(--t0)}svg.diagram .card.tint.t1 .box{fill:var(--t1)}svg.diagram .card.tint.t2 .box{fill:var(--t2)}
svg.diagram .card.tint.t3 .box{fill:var(--t3)}svg.diagram .card.tint.t4 .box{fill:var(--t4)}svg.diagram .card.tint.t5 .box{fill:var(--t5)}
svg.diagram .card.changed .box{stroke:var(--changed);stroke-width:1.6;stroke-dasharray:5 4}
svg.diagram .card.only .box{stroke:var(--only);stroke-width:1.4;stroke-dasharray:3 4;fill:var(--sunken)}
svg.diagram .track{fill:var(--sunken)}
svg.diagram .bar.h0{fill:var(--h0)}svg.diagram .bar.h1{fill:var(--h1)}svg.diagram .bar.h2{fill:var(--h2)}svg.diagram .bar.h3{fill:var(--h3)}svg.diagram .bar.h4{fill:var(--h4)}
svg.diagram text{font-family:var(--sans)}
svg.diagram .kind{font-size:9.5px;font-weight:700;letter-spacing:.07em}
svg.diagram .k-scan{fill:var(--k-scan)}svg.diagram .k-seek{fill:var(--k-seek)}svg.diagram .k-lookup{fill:var(--k-lookup)}svg.diagram .k-join{fill:var(--k-join)}
svg.diagram .k-sort{fill:var(--k-sort)}svg.diagram .k-agg{fill:var(--k-agg)}svg.diagram .k-write{fill:var(--k-write)}svg.diagram .k-dim{fill:var(--k-dim)}
svg.diagram .pct{font-size:11px;font-weight:650;fill:var(--text-2);font-variant-numeric:tabular-nums}
svg.diagram .op{font-size:12.5px;font-weight:650;fill:var(--text)}
svg.diagram .obj{font-family:var(--mono);font-size:10.5px;fill:var(--text-2)}
svg.diagram .rows{font-size:10.5px;fill:var(--text-3)}
svg.diagram .edge{fill:none;stroke:var(--border-strong)}
svg.diagram .edge.hot{stroke:var(--h4);stroke-opacity:.6}
svg.diagram .arrowhead{fill:var(--border-strong)}
svg.diagram .edge-label{font-size:10px;fill:var(--text-3);paint-order:stroke;stroke:var(--surface);stroke-width:5px;stroke-linejoin:round}
svg.diagram .result{font-size:10px;font-weight:700;letter-spacing:.1em;fill:var(--text-3);text-anchor:middle}
@media (max-width:720px){main{padding:24px 16px 40px}.changes li,.scen li{grid-template-columns:1fr;gap:4px}}
@media print{:root{color-scheme:light}body{background:#fff}.wrap{overflow:visible;background:#fff}main{max-width:none;padding:0}}
"#;

fn page(title: &str, chips: &[String], body: &str, options: &ExportOptions) -> String {
    let mut chips_html = String::new();
    for c in chips {
        let _ = write!(chips_html, r#"<span class="chip">{}</span>"#, escape(c));
    }
    if options.redact {
        let _ = write!(
            chips_html,
            r#"<span class="chip redacted" title="{}">{}</span>"#,
            escape(&t!("engine.export.page.redacted_title")),
            escape(&t!("engine.export.page.redacted_chip"))
        );
    }
    format!(
        r#"<!doctype html>
<html lang="{}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><meta name="generator" content="Culprit"><title>{}</title><style>{STYLE}</style></head>
<body><main><div class="top"><span class="mark">C</span><span class="brand">Culprit</span><span class="spacer"></span>{chips_html}</div>
{body}
<footer><span>{}</span>{}</footer></main></body></html>
"#,
        escape(&culprit_core::i18n::current().code),
        escape(title),
        escape(&t!("engine.export.page.footer", when = exported_at())),
        if options.redact {
            format!("<span>{}</span>", escape(&t!("engine.export.page.footer_redacted")))
        } else {
            String::new()
        }
    )
}

fn strip(plan: &Plan) -> String {
    let costs = top_costs(plan, 6);
    // A plan that states no share for any step (no costs, or a step it doesn't cost) has no strip, only why.
    let note = shares_note(plan, None)
        .or_else(|| (!plan.weighed()).then(|| t!("engine.export.page.no_costs")))
        .map(|n| format!(r#"<p class="note">{}</p>"#, escape(&n.replace('`', ""))))
        .unwrap_or_default();
    if costs.is_empty() {
        return note;
    }
    let mut bar =
        format!(r#"<div class="strip" role="img" aria-label="{}">"#, attribute(&t!("engine.export.page.strip_label")));
    let mut legend = String::from(r#"<ul class="legend">"#);
    for (n, s) in &costs {
        let _ = write!(
            bar,
            r#"<span class="{}" style="flex:{:.4}" title="{}"></span>"#,
            heat_class(*s),
            s,
            escape(&format!("{} {}", node_label(n), format::percent(*s)))
        );
        let _ = write!(
            legend,
            r#"<li><span class="sw {}"></span><b>{}</b> {}</li>"#,
            heat_class(*s),
            format::percent(*s),
            escape(&node_label(n))
        );
    }
    let rest: f64 = 1.0 - costs.iter().map(|(_, s)| s).sum::<f64>();
    if rest > 0.005 {
        let _ = write!(bar, r#"<span class="h0" style="flex:{rest:.4}"></span>"#);
    }
    bar.push_str("</div>");
    legend.push_str("</ul>");
    format!("{bar}{legend}{note}")
}

fn steps(plan: &Plan) -> String {
    let mut rows = Vec::new();
    fn walk<'a>(n: &'a PlanNode, depth: usize, out: &mut Vec<(&'a PlanNode, usize)>) {
        out.push((n, depth));
        n.children.iter().for_each(|c| walk(c, depth + 1, out));
    }
    walk(&plan.root, 0, &mut rows);
    let hot = plan.hottest().map(|n| n.id);
    let mut out = format!(
        r#"<table class="steps"><thead><tr><th>{}</th><th>{}</th><th class="r">{}</th><th class="r">{}</th><th>{}</th></tr></thead><tbody>"#,
        escape(&t!("engine.export.column.step")),
        escape(&t!("engine.export.column.table_index")),
        escape(&t!("engine.export.column.cost")),
        escape(&rows_column(plan)),
        escape(&t!("engine.export.column.notes"))
    );
    for (n, depth) in rows {
        // No share where the plan can't state one (no costs, a step it doesn't cost, a default guess).
        let stated = share(plan, n);
        let s = stated.unwrap_or(0.0);
        let warnings: Vec<String> = n.warnings.iter().map(|w| w.label()).collect();
        let _ = write!(
            out,
            r#"<tr{}><td class="op" style="padding-left:{}px">{}</td><td class="obj">{}</td><td class="r"><span class="pctbar"><i class="{}" style="width:{:.0}%"></i></span>{}</td><td class="r">{}</td><td class="warnings">{}</td></tr>"#,
            if Some(n.id) == hot && s >= 0.05 { r#" class="hotrow""# } else { "" },
            12 + depth * 16,
            escape(&n.op),
            escape(&where_(n)),
            heat_class(s),
            (s * 100.0).clamp(0.0, 100.0),
            stated.map(format::percent).unwrap_or_else(|| "—".into()),
            escape(&rows_text(n)),
            escape(&warnings.join(", "))
        );
    }
    out.push_str("</tbody></table>");
    out
}

fn statement(plan: &Plan) -> String {
    match &plan.statement {
        Some(s) => format!("<h2>{}</h2><pre>{}</pre>", escape(&t!("engine.export.statement")), escape(s.trim())),
        None => String::new(),
    }
}

fn notes_html(notes: &[ExportNote], plan: &Plan) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = format!(r#"<h2>{}</h2><ul class="scen">"#, escape(&t!("engine.export.insights")));
    for n in notes {
        let on = n.node.and_then(|id| plan.nodes().into_iter().find(|x| x.id == id)).map(node_label);
        let _ = write!(
            out,
            r#"<li><span class="tag">{}</span><div><div class="t">{}</div>{}{}</div></li>"#,
            escape(&on.unwrap_or_else(|| t!("engine.export.plan"))),
            escape(&n.title),
            n.detail.as_deref().map(|d| format!(r#"<div class="d">{}</div>"#, escape(d))).unwrap_or_default(),
            n.fix
                .as_deref()
                .map(|f| format!(
                    r#"<div class="d"><b>{}</b> {}</div>"#,
                    escape(&t!("engine.export.fix_label")),
                    escape(f)
                ))
                .unwrap_or_default()
        );
    }
    out.push_str("</ul>");
    out
}

fn plan_figures(plan: &Plan) -> String {
    let mut out = String::from(r#"<div class="figures">"#);
    if plan.weighed() || plan.facts().estimated_cost_unavailable {
        out.push_str(&figure(&estimated_cost(plan), &t!("engine.export.page.figure.estimated_cost")));
    }
    out.push_str(&figure(&plan.nodes().len().to_string(), &t!("engine.export.page.figure.operators")));
    if let Some(time) = plan.root.act_time_ms {
        out.push_str(&figure(&format::duration(time), &t!("engine.export.page.figure.actual_time")));
    }
    if let Some((hot, s)) = plan.hottest().and_then(|hot| Some((hot, share(plan, hot)?))) {
        let label = t!("engine.export.page.figure.in_step", step = node_label(hot));
        out.push_str(&figure(&format::percent(s), &escape(&label)));
    }
    out.push_str("</div>");
    out
}

/// Catalog text inside a double-quoted attribute: only a double quote needs escaping there.
fn attribute(text: &str) -> String {
    text.replace('"', "&quot;")
}

/// One of a page's figures: a value with its label under it (both already HTML).
fn figure(value: &str, label: &str) -> String {
    format!(r#"<div class="fig"><div class="v">{value}</div><div class="l">{label}</div></div>"#)
}

/// One plan: its numbers, where the cost goes, the diagram, every step, notes and the statement.
pub fn plan_html(plan: &Plan, title: &str, options: &ExportOptions) -> String {
    let plan = prepared(plan, options);
    let mut body = String::new();
    let _ = write!(body, "<h1>{}</h1>", escape(title));
    let _ =
        write!(body, r#"<p class="sub">{} · {}</p>"#, escape(plan_engine_label(&plan)), escape(&kind_label(plan.kind)));
    body.push_str(&plan_figures(&plan));
    let _ = write!(body, "<h2>{}</h2>{}", escape(&t!("engine.export.where_cost_goes")), strip(&plan));
    let _ = write!(
        body,
        r#"<h2>{}</h2><div class="wrap">{}</div>"#,
        escape(&t!("engine.export.plan")),
        diagram(&plan, &HashMap::new(), None, title, "p")
    );
    let _ = write!(body, "<h2>{}</h2>{}", escape(&t!("engine.export.steps")), steps(&plan));
    body.push_str(&notes_html(&options.notes, &plan));
    body.push_str(&statement(&plan));
    page(title, &[plan_engine_label(&plan).to_string(), kind_label(plan.kind)], &body, options)
}

/// A change's class on the page.
fn change_class(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Access => "access",
        ChangeKind::Operator => "operator",
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
        ChangeKind::Cost => "cost",
        ChangeKind::Rows => "rows",
        ChangeKind::Warning => "warning",
    }
}

fn scenario_tag(kind: ScenarioKind) -> String {
    match kind {
        ScenarioKind::Statement => t!("engine.export.scenario.statement"),
        ScenarioKind::PlanKind => t!("engine.export.scenario.plan_kind"),
        ScenarioKind::Estimator => t!("engine.export.scenario.estimator"),
        ScenarioKind::Parameters => t!("engine.export.scenario.parameters"),
        ScenarioKind::Estimates => t!("engine.export.scenario.estimates"),
        ScenarioKind::Index => t!("engine.export.scenario.index"),
        ScenarioKind::Conversion => t!("engine.export.scenario.types"),
        ScenarioKind::Parallelism => t!("engine.export.scenario.parallelism"),
        ScenarioKind::Memory => t!("engine.export.scenario.memory"),
    }
}

/// "3.4× slower" in red, "2× faster" in green, or "about the same", from actual time when both ran, else cost.
fn verdict(diff: &PlanDiff) -> (&'static str, String) {
    let (r, time) = match (diff.time_a_ms, diff.time_b_ms) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => (b / a, true),
        _ => match diff.cost_ratio {
            Some(r) => (r, false),
            None => return ("same", t!("engine.export.verdict.not_comparable")),
        },
    };
    let x = |v: f64| {
        if v >= 10.0 {
            t!("common.factor", n = format::count(v.round()))
        } else {
            t!("common.factor", n = format::digits(format!("{v:.1}")))
        }
    };
    if r >= 1.25 {
        let factor = x(r);
        let word = if time {
            t!("engine.export.verdict.slower", factor = factor)
        } else {
            t!("engine.export.verdict.costlier", factor = factor)
        };
        ("worse", word)
    } else if r <= 0.8 {
        let factor = x(1.0 / r);
        let word = if time {
            t!("engine.export.verdict.faster", factor = factor)
        } else {
            t!("engine.export.verdict.cheaper", factor = factor)
        };
        ("better", word)
    } else {
        ("same", t!("engine.export.verdict.same"))
    }
}

fn diff_marks(diff: &PlanDiff) -> (HashMap<u32, Mark>, HashMap<u32, Mark>) {
    let (mut a, mut b) = (HashMap::new(), HashMap::new());
    for (i, r) in diff.regions.iter().enumerate() {
        r.a.iter().for_each(|id| {
            a.insert(*id, Mark::Region(i));
        });
        r.b.iter().for_each(|id| {
            b.insert(*id, Mark::Region(i));
        });
    }
    for c in diff.changes.iter().filter(|c| matches!(c.kind, ChangeKind::Access | ChangeKind::Operator)) {
        if let Some(id) = c.a {
            a.insert(id, Mark::Changed);
        }
        if let Some(id) = c.b {
            b.insert(id, Mark::Changed);
        }
    }
    diff.only_a.iter().for_each(|id| {
        a.insert(*id, Mark::Only);
    });
    diff.only_b.iter().for_each(|id| {
        b.insert(*id, Mark::Only);
    });
    (a, b)
}

/// Two plans compared: the verdict and why, the numbers side by side, what changed, the conditions that differ, and
/// both diagrams with identical regions tinted alike.
pub fn diff_html(a: &Plan, b: &Plan, diff: &PlanDiff, name_a: &str, name_b: &str, options: &ExportOptions) -> String {
    let (a, b) = (prepared(a, options), prepared(b, options));
    let redacted_diff = options.redact.then(|| culprit_core::plan_diff_named(&a, name_a, &b, name_b));
    let diff = redacted_diff.as_ref().unwrap_or(diff);
    let title = t!("engine.export.plan_comparison", a = name_a, b = name_b);
    let mut body = String::new();
    let _ = write!(body, "<h1>{}</h1>", escape(&title));
    let (class, word) = verdict(diff);
    if let Some((lead, rest)) = diff.headlines.split_first() {
        let _ = write!(
            body,
            r#"<p class="lead"><span class="verdict {class}">{}</span>{}</p>"#,
            escape(&word),
            escape(lead)
        );
        if !rest.is_empty() {
            body.push_str(r#"<ul class="more">"#);
            for h in rest {
                let _ = write!(body, "<li>{}</li>", escape(h));
            }
            body.push_str("</ul>");
        }
    }
    body.push_str(r#"<div class="figures">"#);
    for (plan, name) in [(&a, name_a), (&b, name_b)] {
        let label = t!("engine.export.page.figure.cost_of", name = name);
        body.push_str(&figure(&estimated_cost(plan), &escape(&label)));
    }
    if let (Some(ta), Some(tb)) = (diff.time_a_ms, diff.time_b_ms) {
        for (time, name) in [(ta, name_a), (tb, name_b)] {
            let label = t!("engine.export.page.figure.actual_time_of", name = name);
            body.push_str(&figure(&format::duration(time), &escape(&label)));
        }
    }
    body.push_str(&figure(
        &format!("{} → {}", a.nodes().len(), b.nodes().len()),
        &escape(&t!("engine.export.page.figure.operators")),
    ));
    body.push_str("</div>");
    if !diff.changes.is_empty() {
        let _ = write!(body, r#"<h2>{}</h2><ul class="changes">"#, escape(&t!("engine.export.what_changed")));
        for c in diff.changes.iter().take(24) {
            let (cls, label) = (change_class(c.kind), escape(&change_word(c.kind)));
            let _ = write!(
                body,
                r#"<li><span class="tag {cls}">{label}</span><div><div class="t">{}</div>{}</div></li>"#,
                escape(&c.title),
                c.detail.as_deref().map(|d| format!(r#"<div class="d">{}</div>"#, escape(d))).unwrap_or_default()
            );
        }
        body.push_str("</ul>");
    }
    if !diff.scenarios.is_empty() {
        let _ = write!(body, r#"<h2>{}</h2><ul class="scen">"#, escape(&t!("engine.export.why_differently")));
        for s in &diff.scenarios {
            let _ = write!(
                body,
                r#"<li><span class="tag">{}</span><div><div class="t">{}</div>{}</div></li>"#,
                escape(&scenario_tag(s.kind)),
                escape(&s.title),
                s.detail.as_deref().map(|d| format!(r#"<div class="d">{}</div>"#, escape(d))).unwrap_or_default()
            );
        }
        body.push_str("</ul>");
    }
    let (marks_a, marks_b) = diff_marks(diff);
    let _ = write!(
        body,
        r#"<h2>{}</h2><div class="keys"><span class="key"><i class="tint"></i>{}</span><span class="key"><i class="changed"></i>{}</span><span class="key"><i class="only"></i>{}</span></div>"#,
        escape(&t!("engine.export.plans")),
        escape(&t!("engine.export.page.key.same")),
        escape(&t!("engine.export.page.key.changed")),
        escape(&t!("engine.export.page.key.only"))
    );
    for (badge, name, plan, marks, id) in [("A", name_a, &a, &marks_a, "a"), ("B", name_b, &b, &marks_b, "b")] {
        let line = t!(
            "engine.export.page.side_line",
            engine = escape(plan_engine_label(plan)),
            kind = kind_inline(plan.kind),
            cost = estimated_cost(plan)
        );
        let _ = write!(
            body,
            r#"<div class="side"><div class="side-head"><span class="badge">{badge}</span><b>{}</b><span>{line}</span></div><div class="wrap">{}</div></div>"#,
            escape(name),
            diagram(plan, marks, None, name, id)
        );
    }
    body.push_str(&notes_html(&options.notes, &b));
    if let Some(s) = b.statement.as_deref().or(a.statement.as_deref()) {
        let _ = write!(body, "<h2>{}</h2><pre>{}</pre>", escape(&t!("engine.export.statement")), escape(s.trim()));
    }
    page(&title, &[plan_engine_label(&b).to_string()], &body, options)
}

fn severity(f: &Finding) -> &'static str {
    if !f.is_actionable() {
        return "low";
    }
    match f.severity {
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
    }
}

/// `shared` is the report checked from its redacted snapshot when the export redacts (see `shared_report`).
fn finding_body(
    out: &mut String,
    report: &Report,
    shared: Option<&Report>,
    finding: &Finding,
    options: &ExportOptions,
    level: &str,
    index: usize,
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
    let _ = write!(out, r#"<section class="finding"><{level}>{}</{level}>"#, escape(&finding.title));
    let _ = write!(
        out,
        r#"<p class="sub"><span class="sev {}"></span>{}</p>"#,
        severity(finding),
        escape(&finding_line(report, finding, finding.impact.label.clone()))
    );
    let _ = write!(out, "<p>{}</p>", escape(&finding.summary));
    if !finding.metrics.is_empty() {
        out.push_str(r#"<div class="metrics">"#);
        for m in &finding.metrics {
            if matches!(m.unit, Unit::Text) {
                let _ = write!(
                    out,
                    r#"<div class="fig metric-text"><div class="l">{}</div><div class="v">{}</div></div>"#,
                    escape(&m.label),
                    escape(&metric_text(m))
                );
                continue;
            }
            let _ = write!(
                out,
                r#"<div class="fig"><div class="v">{}</div><div class="l">{}</div></div>"#,
                escape(&metric_text(m)),
                escape(&m.label)
            );
        }
        out.push_str("</div>");
    }
    let engine = s.server.engine;
    let queries: Vec<_> =
        finding.queries.iter().filter_map(|r| s.workload.as_ref()?.query(&r.id).map(|q| (r, q))).take(3).collect();
    for (qi, (r, q)) in queries.iter().enumerate() {
        let w = s.workload.as_ref().expect("filtered on workload");
        let _ = write!(
            out,
            r#"<h2>{}</h2><p class="qmeta">{}</p><pre>{}</pre>"#,
            escape(&t!("engine.export.query", id = &q.id)),
            escape(&stats_line(w, q)),
            escape(&sql_text(&q.text, engine, options))
        );
        if let Some(plan) = &q.plan {
            let plan = prepared(plan, options);
            // The step the finding names, else its plan's working step where the finding is about one.
            let focus = culprit_checks::finding_step(finding, r, &plan).map(|n| n.id);
            let label = t!("engine.export.page.plan_of_query", id = &q.id);
            let _ = write!(
                out,
                r#"<div class="wrap" style="margin-top:12px">{}</div>"#,
                diagram(&plan, &HashMap::new(), focus, &label, &format!("f{index}q{qi}"))
            );
            if let Some(node) = focus.and_then(|id| plan.nodes().into_iter().find(|n| n.id == id)) {
                let step = format!("<b>{}</b>", escape(&node_label(node)));
                let predicate = node
                    .predicate
                    .as_deref()
                    .or(node.seek_predicate.as_deref())
                    .map(|p| format!("<code>{}</code>", escape(&format::predicate(p))));
                // A share only where the plan can state one, and rows only where it estimates them.
                let share = plan.weighed().then(|| culprit_checks::finding_step_share(&plan, node));
                let rows = || escape(&rows_phrase(node));
                let said = match (share, predicate) {
                    (None, None) => t!("engine.export.page.step.plain", step = step),
                    (None, Some(predicate)) => t!("engine.export.page.step.on", step = step, predicate = predicate),
                    (Some(None), None) => t!("engine.export.page.step.rows", step = step, rows = rows()),
                    (Some(None), Some(predicate)) => {
                        t!("engine.export.page.step.rows_on", step = step, rows = rows(), predicate = predicate)
                    }
                    (Some(Some(s)), None) => {
                        t!("engine.export.page.step.share_rows", step = step, share = format::percent(s), rows = rows())
                    }
                    (Some(Some(s)), Some(predicate)) => t!(
                        "engine.export.page.step.share_rows_on",
                        step = step,
                        share = format::percent(s),
                        rows = rows(),
                        predicate = predicate
                    ),
                };
                let why = culprit_checks::finding_no_share_reason(&plan, node, Some(q))
                    .map(|w| format!(" {}", escape(&w.replace('`', ""))))
                    .unwrap_or_default();
                let _ = write!(out, r#"<p class="note" style="margin-top:8px">{said}{why}</p>"#);
            }
        }
    }
    if let Some(fix) = &finding.fix {
        let _ = write!(
            out,
            r#"<h2>{}</h2><div class="callout"><p>{}</p>"#,
            escape(&t!("engine.export.fix")),
            escape(&fix.summary)
        );
        if let Some(sql) = &fix.sql {
            let _ = write!(out, "<pre>{}</pre>", escape(sql.trim()));
        }
        if let Some(c) = &fix.cost {
            let _ = write!(
                out,
                r#"<p class="note" style="margin:10px 0 0">{}</p>"#,
                escape(&t!("engine.export.cost", cost = c))
            );
        }
        out.push_str("</div>");
    }
    if !finding.verify.is_empty() {
        let _ = write!(out, r#"<h2>{}</h2><ol class="verify">"#, escape(&t!("engine.export.how_to_verify")));
        for step in &finding.verify {
            let _ = write!(out, "<li>{}</li>", escape(step));
        }
        // Only where a re-scan can show it gone: not for statements that stopped, nor on running totals.
        if culprit_checks::gone_after_rescan(shared.unwrap_or(report), finding) {
            let command = format!("<code>culprit verify &lt;connection&gt; {}</code>", escape(&finding.id));
            let _ = write!(out, "<li>{}</li>", t!("engine.export.page.verify_after", command = command));
        }
        out.push_str("</ol>");
    }
    if !finding.caveats.is_empty() {
        let _ = write!(out, r#"<h2>{}</h2><ul class="more">"#, escape(&t!("engine.export.before_you_act")));
        for c in &finding.caveats {
            let _ = write!(out, "<li>{}</li>", escape(c));
        }
        out.push_str("</ul>");
    }
    if let Some(plan) = queries.first().and_then(|(_, q)| q.plan.as_ref()) {
        out.push_str(&notes_html(&options.notes, plan));
    }
    out.push_str("</section>");
}

/// One finding as a page: what's happening, the evidence, the query with its plan (the step to look at outlined), the
/// fix and how to verify it.
pub fn finding_html(report: &Report, finding: &Finding, options: &ExportOptions) -> String {
    let mut body = String::new();
    let shared = options.redact.then(|| super::shared_report(report));
    let title = match &shared {
        Some(shared) => super::shareable(report, shared, finding).title,
        None => finding.title.clone(),
    };
    finding_body(&mut body, report, shared.as_ref(), finding, options, "h1", 0);
    let chip = t!("engine.export.page.finding_chip", id = &finding.id);
    page(&title, &[report_engine_label(report).to_string(), chip], &body, options)
}

/// Every finding of a scan on one page, highest impact first.
pub fn report_html(report: &Report, options: &ExportOptions) -> String {
    let n = report.findings.len();
    let mut body = format!(
        r#"<h1>{}</h1><p class="sub">{}</p>"#,
        escape(&t!("engine.export.page.opportunities", n = n, count = n.to_string())),
        escape(&t!("engine.export.page.report_sub", server = server_line(report), when = &report.snapshot.taken_at))
    );
    if n == 0 {
        let _ = write!(body, "<p>{}</p>", escape(&t!("engine.export.nothing_stands_out")));
    } else {
        let shared = options.redact.then(|| super::shared_report(report));
        body.push_str(r#"<ul class="scen" style="margin-top:18px">"#);
        for f in &report.findings {
            let title = match &shared {
                Some(shared) => super::shareable(report, shared, f).title,
                None => f.title.clone(),
            };
            let _ = write!(
                body,
                r#"<li><span class="tag">{}</span><div><div class="t"><span class="sev {}"></span>{}</div></div></li>"#,
                escape(&f.impact.label),
                severity(f),
                escape(&title)
            );
        }
        body.push_str("</ul>");
        for (i, f) in report.findings.iter().take(10).enumerate() {
            finding_body(&mut body, report, shared.as_ref(), f, options, "h1", i + 1);
        }
    }
    page(&t!("engine.export.page.report_title"), &[report_engine_label(report).to_string()], &body, options)
}
