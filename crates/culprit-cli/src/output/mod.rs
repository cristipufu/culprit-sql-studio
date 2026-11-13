mod diff;
pub(crate) mod observations;
pub(crate) mod sarif;
#[cfg(test)]
mod tests;
mod views;

use std::io::{IsTerminal, Write};
use std::path::Path;

use clap::ValueEnum;
use culprit_core::{
    AuthKind, CheckInfo, Engine, Environment, Error, Finding, LockedCheck, QueryStat, Report, ServerInfo, Severity,
    format, t,
};
use culprit_engine::{ConnectionInfo, DiscoveryResult, ImportCandidate};
use serde_json::{Value, json};

use crate::TopBy;

pub(crate) const BRIEF_FINDING_LIMIT: usize = 3;
pub(crate) const BRIEF_TITLE_CHARS: usize = 160;

pub(crate) fn append_brief_titles<'a>(text: &mut String, titles: impl Iterator<Item = &'a str>) {
    for title in titles.take(BRIEF_FINDING_LIMIT) {
        text.push_str("\n• ");
        text.extend(title.chars().take(BRIEF_TITLE_CHARS));
    }
}

/// "just now", "7 min ago", "2 h 5 min ago": how long ago a scan was taken, as the app says it.
pub(crate) fn age(taken_at: &str) -> Option<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    let seconds = (now - format::epoch_seconds(taken_at)?).max(0.0);
    Some(if seconds < 60.0 {
        t!("cli.age.just_now")
    } else {
        t!("cli.age.ago", duration = format::duration(seconds * 1000.0))
    })
}

/// "production", "staging", "development", "local": an environment as a word in a line.
pub(crate) fn environment_word(environment: Environment) -> String {
    t!("cli.environment", environment = format!("{environment:?}").to_lowercase())
}

/// "Estimated plan": a plan's kind as a label.
pub(crate) fn plan_kind(kind: culprit_core::PlanKind) -> String {
    match kind {
        culprit_core::PlanKind::Estimated => t!("common.plan_kind.estimated"),
        culprit_core::PlanKind::Generic => t!("common.plan_kind.generic"),
        culprit_core::PlanKind::Actual => t!("common.plan_kind.actual"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Json,
    #[value(alias = "markdown")]
    Md,
    Sarif,
}

pub struct Out {
    pub(super) format: Format,
    color: bool,
}

pub(super) struct Style(&'static str);

pub(super) const BOLD: Style = Style("1");
pub(super) const DIM: Style = Style("2");
pub(super) const RED: Style = Style("31");
pub(super) const YELLOW: Style = Style("33");
pub(super) const BLUE: Style = Style("34");
pub(super) const GREEN: Style = Style("32");

impl Out {
    pub fn new(format: Format) -> Out {
        let color = format == Format::Text && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Out { format, color }
    }

    pub(super) fn paint(&self, style: Style, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", style.0)
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Print a value as JSON (for commands whose output is already the right shape).
    pub fn value(&self, value: &Value) {
        self.json(value)
    }

    /// Text styled for a terminal (plain when piped or NO_COLOR is set).
    pub fn bold(&self, text: &str) -> String {
        self.paint(BOLD, text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(DIM, text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint(RED, text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint(YELLOW, text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint(GREEN, text)
    }

    pub fn blue(&self, text: &str) -> String {
        self.paint(BLUE, text)
    }

    pub(super) fn json(&self, value: &Value) {
        let mut stdout = std::io::stdout().lock();
        let _ = serde_json::to_writer_pretty(&mut stdout, value);
        let _ = writeln!(stdout);
    }

    pub fn progress(&self, message: &str) {
        if self.format == Format::Text && std::io::stderr().is_terminal() {
            eprintln!("{}", self.paint(DIM, message));
        }
    }

    pub fn done(&self, message: &str) {
        match self.format {
            Format::Json => self.json(&json!({ "ok": true, "message": message })),
            _ => println!("{message}"),
        }
    }

    pub fn error(&self, error: &Error) {
        if self.format == Format::Json {
            let body = json!({ "error": error });
            eprintln!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            return;
        }
        eprintln!("{} {}", self.paint(RED, &t!("cli.error")), error.message);
        if let Some(hint) = &error.hint {
            eprintln!("{} {hint}", self.paint(DIM, &t!("cli.hint")));
        }
    }

    pub(super) fn severity_mark(&self, s: Severity) -> String {
        match s {
            Severity::High => self.paint(RED, "●"),
            Severity::Medium => self.paint(YELLOW, "●"),
            Severity::Low => self.paint(BLUE, "●"),
        }
    }

    pub fn connections(&self, list: &[ConnectionInfo]) {
        if self.format == Format::Json {
            return self.json(&json!(list));
        }
        if list.is_empty() {
            println!(
                "{} {}\n  culprit conn add <name> --url '<connection string>' --env development",
                t!("cli.connections.none"),
                t!("cli.connections.add_one")
            );
            return;
        }
        for c in list {
            let env = environment_word(c.environment);
            let last = c
                .last_report
                .as_ref()
                .map(|r| {
                    let when = age(&r.taken_at).unwrap_or_else(|| r.taken_at.clone());
                    let n = r.opportunities;
                    t!("cli.connections.scanned", n = n, count = n.to_string(), when = when)
                })
                .unwrap_or_else(|| t!("cli.connections.never_scanned"));
            println!(
                "{}  {:<11}  {}  {}",
                self.paint(BOLD, &format!("{:<20}", c.name)),
                engine_name(c),
                self.paint(DIM, &format!("{:<12}", env)),
                self.paint(DIM, &format!("{}  ·  {last}", c.display))
            );
        }
    }

    /// `server`: what the connection test found, so a MariaDB server is named as itself.
    pub fn connection_added(&self, info: &ConnectionInfo, server: Option<&ServerInfo>) {
        if self.format == Format::Json {
            return self.json(&json!(info));
        }
        let engine = server.map(|s| s.name()).unwrap_or(engine_name(info));
        let name = self.paint(BOLD, &info.name);
        println!("{}", t!("cli.connections.saved", name = name, engine = engine, display = &info.display));
        match (&info.query_log, info.engine.has_server()) {
            (Some(log), _) => println!("{}", t!("cli.connections.query_log", log = log, name = &info.name)),
            (None, false) => println!(
                "{} {}",
                t!("cli.connections.no_query_log"),
                t!("cli.connections.sqlite_history", name = &info.name)
            ),
            (None, true) => {}
        }
        println!("{}", t!("cli.next", command = format!("culprit scan {}", info.name)));
    }

    pub fn connection_renamed(&self, old: &str, info: &ConnectionInfo) {
        if self.format == Format::Json {
            return self.json(&json!(info));
        }
        println!("{}", t!("cli.connections.renamed", old = old, new = self.paint(BOLD, &info.name)));
    }

    pub fn discovered(&self, result: &DiscoveryResult, saved: &[ConnectionInfo]) {
        let found = &result.items;
        if self.format == Format::Json {
            return self.json(&json!({ "databases": found, "saved": saved, "has_more": result.has_more, "warnings": result.warnings }));
        }
        for warning in &result.warnings {
            println!("{warning}");
        }
        if result.has_more {
            let n = found.len();
            println!("{} {}", t!("cli.discover.showing", n = n, count = n.to_string()), t!("cli.discover.refine"));
        }
        if found.is_empty() {
            if result.has_more {
                println!("{} {}", t!("cli.discover.incomplete_none"), t!("cli.discover.narrow"));
            } else {
                println!("{} {}", t!("cli.discover.none"), t!("cli.discover.try_another"));
            }
            return;
        }
        for d in found {
            let env = environment_word(d.environment);
            let auth = match d.auth {
                AuthKind::Entra => t!("cli.auth.entra"),
                AuthKind::AwsIam => t!("cli.auth.aws_iam"),
                AuthKind::Password => t!("cli.auth.password"),
                AuthKind::None => t!("cli.auth.none"),
            };
            let status = match (&d.saved_as, saved.iter().any(|s| s.name == d.alias)) {
                (Some(name), _) => self.paint(DIM, &t!("cli.saved_as", name = name)),
                (None, true) => self.paint(GREEN, &t!("cli.saved")),
                (None, false) => String::new(),
            };
            println!(
                "{}  {}  {:<10}  {}/{} ({}, {})  {}",
                self.paint(BOLD, &format!("{:<28}", d.alias)),
                self.paint(DIM, &format!("{:<11}", env)),
                d.engine.label(),
                d.server,
                if d.database.is_empty() { "—" } else { &d.database },
                d.location,
                auth,
                status
            );
        }
        if !result.has_more
            && result.warnings.is_empty()
            && saved.is_empty()
            && found.iter().any(|d| d.saved_as.is_none())
        {
            println!();
            println!("{}", self.paint(DIM, &t!("cli.discover.save_hint")));
        }
    }

    pub fn imported(&self, found: &[ImportCandidate], saved: &[ConnectionInfo], dry_run: bool) {
        if self.format == Format::Json {
            return self.json(&json!({ "connections": found, "saved": saved, "dry_run": dry_run }));
        }
        if found.is_empty() {
            println!("{} {}", t!("cli.import.none"), t!("cli.import.point_file"));
            return;
        }
        for c in found {
            let env = environment_word(c.environment);
            let status = match (&c.saved_as, saved.iter().any(|s| s.name == c.alias)) {
                (Some(name), _) => self.paint(DIM, &t!("cli.import.already_saved", name = name)),
                (None, true) => self.paint(GREEN, &t!("cli.import.imported")),
                (None, false) => String::new(),
            };
            let target = format!(
                "{}{}{}",
                c.host,
                c.port.map(|p| format!(":{p}")).unwrap_or_default(),
                c.database.as_deref().map(|d| format!("/{d}")).unwrap_or_default()
            );
            println!(
                "{}  {}  {:<10}  {}  {}  {}",
                self.paint(BOLD, &format!("{:<28}", c.alias)),
                self.paint(DIM, &format!("{:<11}", env)),
                c.engine.label(),
                target,
                self.paint(DIM, &format!("({})", c.source.label())),
                status
            );
            if let Some(note) = &c.note {
                println!("    {}", self.paint(YELLOW, note));
            }
        }
        if dry_run && found.iter().any(|c| c.saved_as.is_none()) {
            println!();
            println!("{}", self.paint(DIM, &format!("{} {}", t!("cli.import.dry_run"), t!("cli.import.run_again"))));
        } else if saved.iter().any(|s| s.auth == AuthKind::Password) {
            println!();
            println!("{}", self.paint(DIM, &t!("cli.import.passwords")));
        }
    }

    pub fn server(&self, server: &ServerInfo) {
        if self.format == Format::Json {
            return self.json(&json!(server));
        }
        println!(
            "{} {}{}",
            self.paint(GREEN, "✓"),
            t!("cli.server.line", product = &server.product, database = &server.database),
            server.cloud.as_deref().map(|c| format!(" · {c}")).unwrap_or_default()
        );
    }

    pub fn doctor(&self, info: &ConnectionInfo, server: &ServerInfo, locked: &[LockedCheck]) {
        if self.format == Format::Json {
            return self.json(&json!({ "connection": info, "server": server, "locked": locked }));
        }
        self.server(server);
        println!(
            "  {}{}",
            t!("cli.doctor.version", version = &server.version),
            server.edition.as_deref().map(|e| format!(" · {e}")).unwrap_or_default()
        );
        if let Some(started) = &server.started_at {
            println!("  {}", t!("cli.doctor.counters_since", since = started));
        }
        println!();
        for cap in &server.capabilities {
            println!("  {} {}", self.paint(GREEN, "✓"), cap.label_for(server.engine));
        }
        for m in &server.missing {
            println!("  {} {} — {}", self.paint(RED, "✗"), m.capability.label_for(server.engine), m.reason);
            if let Some(fix) = &m.fix {
                println!("    {}", self.paint(DIM, &fix.replace('\n', "\n    ")));
            }
        }
        if !locked.is_empty() {
            println!();
            println!("{}", self.paint(BOLD, &t!("cli.doctor.locked")));
            for l in locked {
                let needs: Vec<String> = l.missing.iter().map(|c| c.label_for(server.engine).to_string()).collect();
                println!("  {}", t!("cli.doctor.needs", check = &l.check.title, needs = format::list(&needs)));
            }
        }
    }

    pub fn scan(&self, report: &Report, full: bool, dir: &Path) {
        let path = dir.join(format!("{}.json", report.snapshot.id));
        if self.format == Format::Json {
            if full {
                return self.json(&json!(culprit_engine::reports::display(report)));
            }
            return self.json(&compact_report(report, &path));
        }
        if self.format == Format::Sarif {
            let list: Vec<&Finding> = report.findings.iter().collect();
            return self.json(&sarif::findings_sarif(&list, None, None));
        }
        if self.format == Format::Md {
            return print!("{}", markdown_report(report));
        }
        let s = &report.snapshot;
        println!(
            "{}",
            t!(
                "cli.scan.line",
                connection = self.paint(BOLD, &s.connection),
                product = &s.server.product,
                database = &s.server.database,
                duration = format::duration(s.duration_ms)
            )
        );
        if let Some(w) = &s.workload {
            // Under 20 hours of history a per-day figure would be an extrapolation: say what was measured, and over what.
            let time = if w.measurement_hours() < 20.0 {
                t!("cli.scan.time_over", time = format::duration(w.total_time_ms), window = w.window_phrase())
            } else {
                t!("cli.scan.time_per_day", time = format::duration(w.per_day(w.total_time_ms)))
            };
            let queries = t!(
                "cli.scan.queries",
                n = w.query_count,
                count = format::count(w.query_count),
                source = w.source_name()
            );
            println!("{}", self.paint(DIM, &format!("{queries} · {time}")));
        }
        println!();
        self.findings(report, Severity::Low);
        if !s.notes.is_empty() {
            println!();
            for n in &s.notes {
                println!("{} {}", self.paint(YELLOW, "!"), n.message);
            }
        }
        println!();
        println!("{}", self.paint(DIM, &t!("cli.scan.saved", path = path.display().to_string())));
    }

    pub fn analyzed(&self, report: &Report) {
        if self.format == Format::Json {
            return self.json(&json!(culprit_engine::reports::display(report)));
        }
        self.findings(report, Severity::Low);
    }

    pub fn lint_sql(&self, report: &Report, batches: &[(Option<String>, String, Vec<Finding>)]) {
        if self.format == Format::Sarif {
            let refs: Vec<(Option<&str>, Option<&str>, &[Finding])> =
                batches.iter().map(|(uri, sql, fs)| (uri.as_deref(), Some(sql.as_str()), fs.as_slice())).collect();
            return self.json(&sarif::findings_sarif_batches(&refs));
        }
        if self.format == Format::Json {
            return self.json(&json!({ "summary": report.summary, "findings": report.findings }));
        }
        if self.format == Format::Md {
            let mut md = markdown_report(report);
            for (i, f) in report.findings.iter().enumerate() {
                md.push_str(&format!("\n### {}. {}\n\n{}\n", i + 1, f.title, f.summary));
                if let Some(fix) = &f.fix {
                    md.push_str(&format!("\n**{}** {}\n", t!("cli.fix"), fix.summary));
                    if let Some(sql) = &fix.sql {
                        md.push_str(&format!("\n```sql\n{}\n```\n", sql.trim()));
                    }
                }
            }
            return print!("{md}");
        }
        println!("{}", self.paint(BOLD, &report.summary.headline));
        for (i, f) in report.findings.iter().enumerate() {
            println!();
            println!(
                "{:>3}  {}  {}  {}",
                i + 1,
                self.severity_mark(f.severity),
                self.paint(BOLD, &f.title),
                self.paint(DIM, &format!("· {}", f.impact.label))
            );
            println!("       {}", self.paint(DIM, &f.id));
            println!("       {}", f.summary);
            if let Some(fix) = &f.fix {
                println!("       {} {}", self.paint(BOLD, &t!("cli.fix")), fix.summary);
                if let Some(sql) = &fix.sql {
                    for line in sql.lines() {
                        println!("           {}", self.paint(BLUE, line));
                    }
                }
            }
        }
    }

    pub fn findings(&self, report: &Report, min: Severity) {
        let list: Vec<&Finding> = report.findings.iter().filter(|f| f.severity >= min).collect();
        if self.format == Format::Json {
            return self.json(&json!({ "summary": report.summary, "findings": list, "locked": report.locked, "assessments": report.assessments }));
        }
        if self.format == Format::Sarif {
            return self.json(&sarif::findings_sarif(&list, None, None));
        }
        if self.format == Format::Md {
            return print!("{}", markdown_report(report));
        }
        println!("{}", self.paint(BOLD, &report.summary.headline));
        println!();
        // Findings whose queries stopped running come last, under their own heading: likely fixed or changed by a deploy.
        let (active, stopped): (Vec<&Finding>, Vec<&Finding>) = list.iter().partition(|f| f.is_actionable());
        for (i, f) in active.iter().enumerate() {
            println!(
                "{:>3}  {}  {}  {}",
                i + 1,
                self.severity_mark(f.severity),
                f.title,
                self.paint(DIM, &format!("· {}", f.impact.label))
            );
            println!("       {}", self.paint(DIM, &f.id));
        }
        if !stopped.is_empty() {
            println!();
            println!("{}", self.paint(BOLD, &t!("cli.findings.history", count = stopped.len().to_string())));
            for (i, f) in stopped.iter().enumerate() {
                println!("{:>3}  {}  {}", active.len() + i + 1, self.paint(DIM, "○"), self.paint(DIM, &f.title));
                println!("       {}", self.paint(DIM, &f.id));
            }
        }
        let list: Vec<&Finding> = active.iter().chain(stopped.iter()).copied().collect();
        if !report.assessments.is_empty() {
            let assessed =
                report.assessments.iter().filter(|a| a.status == culprit_core::CheckStatus::Assessed).count();
            let limited = report
                .assessments
                .iter()
                .filter(|a| {
                    matches!(
                        a.status,
                        culprit_core::CheckStatus::Unavailable | culprit_core::CheckStatus::InsufficientEvidence
                    ) || (a.status == culprit_core::CheckStatus::Findings && a.reason.is_some())
                })
                .count();
            let coverage = format!(
                "{} {}",
                t!("cli.findings.coverage", assessed = assessed, limited = limited),
                t!("cli.findings.coverage_json")
            );
            println!("\n{}", self.paint(DIM, &coverage));
        }
        if !report.locked.is_empty() {
            println!();
            let names: Vec<String> = report.locked.iter().map(|l| l.check.title.clone()).collect();
            let n = names.len();
            println!(
                "{} {} {}",
                self.paint(YELLOW, "!"),
                t!("cli.findings.locked", n = n, count = n.to_string(), names = format::list(&names)),
                t!("cli.findings.see_doctor", connection = &report.snapshot.connection)
            );
        }
        if let Some(first) = list.first() {
            println!();
            let command = format!("culprit show {} '{}'", report.snapshot.connection, first.id);
            println!("{}", self.paint(DIM, &t!("cli.next", command = command)));
        }
    }

    pub fn show(&self, report: &Report, finding: &Finding) {
        let brief = culprit_engine::agent_brief(report, finding);
        if self.format == Format::Json {
            let queries: Vec<Value> = finding
                .queries
                .iter()
                .filter_map(|r| report.snapshot.workload.as_ref()?.query(&r.id))
                .map(|q| query_json(report, q))
                .collect();
            return self.json(&json!({ "finding": finding, "queries": queries, "brief": brief }));
        }
        print!("{brief}");
    }

    pub fn top(&self, report: &Report, by: TopBy, limit: usize) {
        let Some(w) = &report.snapshot.workload else {
            let why = report.summary.no_workload.clone().unwrap_or_else(|| t!("cli.top.no_history"));
            if self.format == Format::Json {
                return self.json(&json!({ "queries": [], "note": why }));
            }
            println!("{why}");
            return;
        };
        let mut queries: Vec<&QueryStat> = w.queries.iter().collect();
        let key = |q: &QueryStat| match by {
            // Its share as every surface states it: over Culprit's scans where they measured it.
            TopBy::Time => w.statement_share(q),
            TopBy::Cpu => q.cpu_time_ms.unwrap_or(0.0),
            TopBy::Reads => q.logical_reads.unwrap_or(0.0),
            TopBy::Executions => q.executions,
        };
        queries.sort_by(|a, b| key(b).total_cmp(&key(a)));
        queries.truncate(limit);
        if self.format == Format::Json {
            let list: Vec<Value> = queries.iter().map(|q| query_json(report, q)).collect();
            return self.json(&json!({ "source": w.source, "window_hours": w.measurement_hours(), "queries": list }));
        }
        // Under 20 hours of history, runs per day would be an extrapolation: show the runs counted, and over what.
        let short_window = w.measurement_hours() < 20.0;
        let span = format::duration(w.measurement_hours() * 3_600_000.0);
        // On a source of running totals, what Culprit's scans measured of the statements they did, as every surface
        // states it; the source's totals for the rest.
        let scans = queries.iter().filter_map(|q| w.scans_of(q)).max_by(|a, b| a.hours.total_cmp(&b.hours));
        let header = match &scans {
            Some(s) => t!("cli.top.header_scans", source = w.source_name(), scans = s.phrase()),
            None if short_window => t!("cli.top.header_short", source = w.source_name(), window = w.window_phrase()),
            None => t!("cli.top.header", source = w.source_name(), span = span),
        };
        println!("{}", self.paint(DIM, &header));
        for q in queries {
            let text = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let short: String = text.chars().take(90).collect();
            // A burst, or fewer runs than days, has no rate a day: when it ran is said after the statement.
            let burst = w.cadence(q).is_burst();
            let runs = match w.scans_of(q).filter(|_| !burst) {
                Some(s) => Some(s.per_day(s.runs)),
                None if short_window && !burst => Some(q.executions),
                None => w.runs_per_day_of(q),
            };
            let ran =
                if runs.is_none() { self.paint(DIM, &format!("  ({})", w.how_often_of(q))) } else { String::new() };
            println!(
                "{:>5}  {:>8}  {:>9}  {}  {}{ran}",
                format::percent(w.statement_share(q)),
                runs.map_or_else(|| "—".to_string(), format::count),
                format::duration(w.statement_mean_ms(q)),
                self.paint(DIM, &format!("{:<8}", q.id)),
                short
            );
        }
    }

    pub fn sources(&self, engines: &[(Engine, &[culprit_core::Source])]) {
        if self.format == Format::Json {
            let by_engine: serde_json::Map<String, Value> =
                engines.iter().map(|(e, s)| (format!("{e:?}").to_ascii_lowercase(), json!(s))).collect();
            return self.json(&Value::Object(by_engine));
        }
        for (engine, sources) in engines.iter().filter(|(_, s)| !s.is_empty()) {
            println!("{}", self.paint(BOLD, engine.label()));
            for s in *sources {
                let delivers: Vec<String> = s.delivers.iter().map(|c| c.label_for(*engine)).collect();
                println!(
                    "  {:<26}{}",
                    s.name,
                    self.paint(
                        DIM,
                        &if delivers.is_empty() {
                            t!("cli.sources.adds_detail", reads = s.description())
                        } else {
                            format!("{} · {}", s.description(), delivers.join(", "))
                        }
                    )
                );
            }
        }
    }

    pub fn checks(&self, checks: &[CheckInfo]) {
        if self.format == Format::Json {
            return self.json(&json!(checks));
        }
        for c in checks {
            // The MySQL provider serves MariaDB too, and every MySQL check runs on both.
            let engines: Vec<&str> =
                c.engines.iter().map(|e| if *e == Engine::Mysql { "MySQL/MariaDB" } else { e.label() }).collect();
            println!("{}  {}", self.paint(BOLD, &format!("{:<16}", c.id)), c.title);
            println!("{}", self.paint(DIM, &format!("{:<18}{} · {}", "", engines.join(", "), c.description)));
        }
    }
}

pub(super) fn query_json(report: &Report, q: &QueryStat) -> Value {
    let w = report.snapshot.workload.as_ref();
    let hot = q.plan.as_ref().and_then(|p| {
        // Where the plan's work is: none in a plan too cheap for any of its steps to matter, nor where the plan can't
        // state the step's share (beside a step it puts no cost on, or on rows that rest on a default guess).
        let n = culprit_core::insights::working_step(p)?;
        let share = p.step_share(n)?;
        Some(json!({
            "op": n.op,
            "object": n.object,
            "index": n.index,
            "predicate": n.predicate,
            "cost_share": share,
            "warnings": n.warnings,
        }))
    });
    json!({
        "id": q.id,
        "text": q.text,
        "object_name": q.object_name,
        // Share, runs a day and average as every surface states them: over Culprit's scans where they measured it
        // (`measured_over`), with the source's totals beside them where those differ twofold (`totals`).
        "db_time_share": w.map(|w| w.statement_share(q)),
        // Per day only from a day's worth of history (20 hours); `runs` and `window_hours` are what was measured.
        // Null for a burst or fewer runs than days, said in `how_often` by when it ran.
        "runs_per_day": w.and_then(|w| match w.scans_of(q).filter(|_| !w.cadence(q).is_burst()) {
            Some(s) => Some(s.per_day(s.runs)),
            None => w.runs_per_day_of(q),
        }),
        "how_often": w.map(|w| w.cost_of(q).runs),
        "window_hours": w.map(|w| w.measurement_hours()),
        "measured_over": w.and_then(|w| w.scans_of(q)).map(|s| s.phrase()),
        "totals": w.and_then(|w| w.cost_of(q).totals),
        "mean_time_ms": w.map_or(q.mean_time_ms, |w| w.statement_mean_ms(q)),
        "max_time_ms": q.max_time_ms,
        "cpu_time_ms": q.cpu_time_ms,
        "rows_per_run": q.rows_per_execution(),
        "reads_per_run": q.reads_per_execution(),
        "runs": q.executions,
        "plans": q.plans.len(),
        // Fastest run and, where the engine keeps them, percentiles and a run-time histogram.
        "spread": q.spread,
        // Failed and cancelled runs, planning and JIT time, log bytes, estimated row width: null where not kept.
        "costs": q.costs,
        // Per plan, when there are several: how each one's run times spread and what values it was compiled for.
        "plan_spread": (q.plans.len() > 1).then(|| {
            q.plans
                .iter()
                .map(|p| {
                    json!({
                        "plan_id": p.id,
                        "runs": p.executions,
                        "mean_ms": p.mean_time_ms,
                        "min_ms": p.min_time_ms,
                        "max_ms": p.max_time_ms,
                        "stddev_ms": p.stddev_time_ms,
                        "compiled": p.compiled,
                        "variant_query": p.variant_query,
                    })
                })
                .collect::<Vec<_>>()
        }),
        "hottest_operator": hot,
        // Average duration per run in each timeline bucket (null where it didn't run or the source can't tell).
        "trend_mean_ms": q.trend.as_ref().map(|t| {
            (0..t.time_ms.len()).map(|i| t.mean_ms(i).map(|m| (m * 100.0).round() / 100.0)).collect::<Vec<_>>()
        }),
    })
}

pub(super) fn compact_report(report: &Report, path: &Path) -> Value {
    let s = &report.snapshot;
    json!({
        "connection": s.connection,
        "report_id": s.id,
        "report_path": path,
        "taken_at": s.taken_at,
        "duration_ms": s.duration_ms,
        "server": {
            "engine": s.server.engine,
            "product": s.server.product,
            "version": s.server.version,
            "edition": s.server.edition,
            "cloud": s.server.cloud,
            "database": s.server.database,
        },
        "workload": s.workload.as_ref().map(|w| json!({
            "source": w.source,
            "window_hours": w.measurement_hours(),
            "total_time_ms": w.total_time_ms,
            "query_count": w.query_count,
            "total_executions": w.total_executions,
        })),
        "summary": report.summary,
        "findings": report.findings,
        "locked": report.locked,
        "assessments": report.assessments,
        "engine_limitations": s.server.engine.spec().limitations(),
        "notes": s.notes,
        "next": report.findings.iter().take(3).map(|f| format!("culprit show {} '{}'", s.connection, f.id)).collect::<Vec<_>>(),
    })
}

pub(super) fn markdown_report(report: &Report) -> String {
    let s = &report.snapshot;
    let mut out = format!(
        "# {}\n\n{}\n\n**{}**\n\n",
        t!("cli.md.report_title", connection = &s.connection),
        t!(
            "cli.md.report_line",
            product = &s.server.product,
            database = format!("`{}`", s.server.database),
            when = culprit_core::changes::dated_time(&s.taken_at)
        ),
        report.summary.headline
    );
    let (active, stopped): (Vec<&Finding>, Vec<&Finding>) = report.findings.iter().partition(|f| f.is_actionable());
    let table = |out: &mut String, list: &[&Finding], from: usize| {
        out.push_str(&format!(
            "| # | {} | {} | {} | {} |\n|---|---|---|---|---|\n",
            t!("cli.md.status"),
            t!("cli.md.finding"),
            t!("cli.md.impact"),
            t!("cli.md.id")
        ));
        for (i, f) in list.iter().enumerate() {
            out.push_str(&format!(
                "| {} | {} | {} | {} | `{}` |\n",
                from + i + 1,
                if f.inactive { t!("cli.stopped_running") } else { f.actionability.label() },
                md_cell(&f.title),
                md_cell(&f.impact.label),
                f.id.replace('`', "'")
            ));
        }
    };
    table(&mut out, &active, 0);
    if !stopped.is_empty() {
        out.push_str(&format!("\n## {}\n\n", t!("cli.findings.history", count = stopped.len().to_string())));
        table(&mut out, &stopped, active.len());
    }
    out
}

/// The engine as people know it: a MySQL connection whose last scan met MariaDB is MariaDB.
pub(super) fn engine_name(c: &ConnectionInfo) -> &'static str {
    let mariadb = c.last_report.as_ref().and_then(|r| r.product.as_deref()).is_some_and(|p| p.contains("MariaDB"));
    if c.engine == Engine::Mysql && mariadb { "MariaDB" } else { c.engine.label() }
}

/// Database text in a Markdown table cell: pipes would split the row, and HTML would render where the report is pasted.
pub(super) fn md_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('<', "&lt;").replace('>', "&gt;").replace('\n', " ")
}

pub(super) fn kb(value: f64) -> String {
    if value >= 1024.0 * 1024.0 {
        t!("common.bytes.gb", n = format::digits(format!("{:.1}", value / 1024.0 / 1024.0)))
    } else if value >= 1024.0 {
        t!("common.bytes.mb", n = format::digits(format!("{:.1}", value / 1024.0)))
    } else {
        t!("common.bytes.kb", n = format::digits(format!("{value:.0}")))
    }
}

pub(super) fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}
