//! The change timeline: `culprit mark` puts a deploy or a migration on a connection's timeline, `culprit events` lists
//! what changed around it (marks, schema, indexes, settings, restarts, throttling), `culprit cloud` shows what the cloud
//! measured and when it held the server back.

use clap::Args;
use culprit_core::changes::short_time;
use culprit_core::{ChangeEvent, CloudSignals, CloudUnit, Error, EventKind, EventSource, Result, format, t};
use culprit_engine::{EventLog, EventsRequest, Workspace};
use serde_json::json;

use crate::output::{Format, Out};

#[derive(Args)]
pub struct MarkArgs {
    /// A saved connection name.
    pub connection: String,
    /// What happened, e.g. `deploy v1.4.2` or `migration 0042 applied`.
    pub text: Vec<String>,
    /// When it happened (RFC 3339), if not now.
    #[arg(long)]
    pub at: Option<String>,
    /// List the connection's marks instead of adding one.
    #[arg(long, conflicts_with_all = ["text", "remove"])]
    pub list: bool,
    /// Remove the mark with this id.
    #[arg(long, conflicts_with = "text")]
    pub remove: Option<String>,
}

#[derive(Args)]
pub struct PeriodArgs {
    /// A saved connection name.
    pub connection: String,
    /// The last N hours: `6h`, `90m` or `2d` (default 7 days).
    #[arg(long, value_parser = crate::parse_hours, conflicts_with = "since")]
    pub last: Option<f64>,
    /// From this moment (RFC 3339).
    #[arg(long)]
    pub since: Option<String>,
    /// Until this moment (RFC 3339; default now).
    #[arg(long)]
    pub until: Option<String>,
    /// Skip cloud metrics (CloudWatch, Azure Monitor).
    #[arg(long)]
    pub no_cloud: bool,
    /// Fetch cloud metrics again instead of reusing the last few minutes' answer.
    #[arg(long)]
    pub refresh: bool,
}

impl PeriodArgs {
    fn request(&self) -> EventsRequest {
        EventsRequest {
            start: self.since.clone(),
            end: self.until.clone(),
            hours: self.last,
            cloud: self.no_cloud.then_some(false),
            refresh: self.refresh,
        }
    }
}

pub fn mark(ws: &Workspace, args: MarkArgs, out: &Out) -> Result<u8> {
    if args.list {
        let marks = ws.marks(&args.connection)?;
        if out.format() == Format::Json {
            out.value(&json!(marks));
        } else if marks.is_empty() {
            println!(
                "{} {}",
                t!("cli.mark.none", connection = &args.connection),
                t!("cli.mark.add_one", command = format!("culprit mark {} deploy v1.4.2", args.connection))
            );
        } else {
            for m in &marks {
                println!("{}  {}  {}", out.dim(&m.id), short_time(&m.at), m.text);
            }
        }
        return Ok(0);
    }
    if let Some(id) = &args.remove {
        let removed = ws.unmark(&args.connection, id)?;
        if out.format() == Format::Json {
            out.value(&json!({ "removed": removed }));
        } else {
            println!("{}", t!("cli.mark.removed", text = &removed.text, connection = &args.connection));
        }
        return Ok(0);
    }
    let text = args.text.join(" ");
    if text.trim().is_empty() {
        return Err(Error::invalid(t!("cli.mark.say_what")));
    }
    let mark = ws.mark(&args.connection, &text, args.at.as_deref(), "cli")?;
    if out.format() == Format::Json {
        out.value(&json!(mark));
    } else {
        println!(
            "{} {}",
            t!("cli.mark.marked", connection = &args.connection, at = short_time(&mark.at), text = &mark.text),
            out.dim(&format!("({})", mark.id))
        );
        let command = format!("culprit changes {} --since {}", args.connection, mark.at);
        println!("{}", out.dim(&t!("cli.mark.compare", command = command)));
    }
    Ok(0)
}

pub async fn events(ws: &Workspace, args: PeriodArgs, out: &Out) -> Result<u8> {
    let log = ws.events(&args.connection, &args.request()).await?;
    if out.format() == Format::Json {
        out.value(&json!(log));
        return Ok(0);
    }
    print_events(&log, out);
    Ok(0)
}

fn print_events(log: &EventLog, out: &Out) {
    let (from, to) = culprit_core::changes::range_times(&log.start, &log.end);
    println!("{}", out.bold(&t!("cli.events.title", connection = &log.connection, from = from, to = to)));
    if log.events.is_empty() {
        println!("{}", t!("cli.events.nothing"));
    }
    for e in log.events.iter().filter(|e| !e.minor) {
        print_event(e, out);
    }
    let minor = log.events.iter().filter(|e| e.minor).count();
    if minor > 0 {
        println!("{}", out.dim(&t!("cli.events.minor", n = minor, count = minor.to_string())));
    }
    for note in &log.notes {
        println!("{}", out.dim(note));
    }
}

fn label(kind: EventKind) -> String {
    match kind {
        EventKind::Mark => t!("cli.events.kind.mark"),
        EventKind::Index => t!("cli.events.kind.index"),
        EventKind::Table => t!("cli.events.kind.table"),
        EventKind::Code => t!("cli.events.kind.code"),
        EventKind::Setting => t!("cli.events.kind.setting"),
        EventKind::Version => t!("cli.events.kind.version"),
        EventKind::Restart => t!("cli.events.kind.restart"),
        EventKind::StatsReset => t!("cli.events.kind.reset"),
        EventKind::Statistics => t!("cli.events.kind.stats"),
        EventKind::Throttle => t!("cli.events.kind.cloud"),
    }
}

fn print_event(e: &ChangeEvent, out: &Out) {
    let tag = format!("{:<8}", label(e.kind));
    let tag = if e.kind == EventKind::Throttle { out.red(&tag) } else { out.dim(&tag) };
    let source = match (e.source, &e.by) {
        (EventSource::Mark, Some(by)) => format!(" · {}", t!("cli.events.by", by = by)),
        (EventSource::Scans, _) => format!(" · {}", t!("cli.events.between_scans")),
        _ => String::new(),
    };
    println!("{tag} {}  {}", e.title, out.dim(&format!("{}{source}", e.when())));
    if let Some(detail) = &e.detail {
        println!("         {}", out.dim(detail));
    }
}

pub async fn cloud(ws: &Workspace, args: PeriodArgs, out: &Out) -> Result<u8> {
    let mut request = args.request();
    request.cloud = Some(true);
    if request.start.is_none() && request.hours.is_none() {
        request.hours = Some(24.0);
    }
    let signals = ws.cloud(&args.connection, &request).await?;
    if out.format() == Format::Json {
        out.value(&json!(signals));
        return Ok(0);
    }
    print_cloud(&signals, out);
    Ok(0)
}

fn print_cloud(s: &CloudSignals, out: &Out) {
    let tier = s.tier.as_deref().map(|t| format!(" · {t}")).unwrap_or_default();
    println!("{}", out.bold(&format!("{} · {}{tier}", s.service, s.resource)));
    println!("{}\n", s.summary);
    for e in &s.episodes {
        let until = if e.ongoing { t!("cli.cloud.now") } else { short_time(&e.end) };
        println!("{} {}  {}", out.red("●"), e.title, out.dim(&format!("{} → {until}", short_time(&e.start))));
        println!("  {}", out.dim(&e.detail));
    }
    if !s.episodes.is_empty() {
        println!();
    }
    for series in &s.series {
        let values: Vec<f64> = series.points.iter().map(|p| p.value).collect();
        if values.is_empty() {
            continue;
        }
        let show = |v: f64| match series.unit {
            CloudUnit::Percent => t!("common.percent", n = format!("{v:.0}")),
            CloudUnit::Ms => format::duration(v),
            CloudUnit::Bytes => format::bytes(v),
            CloudUnit::Credits => format::digits(format!("{v:.1}")),
            CloudUnit::Iops | CloudUnit::Sessions if v < 1.0 => format::digits(format!("{v:.2}")),
            CloudUnit::Iops | CloudUnit::Sessions if v < 10.0 => format::digits(format!("{v:.1}")),
            CloudUnit::Iops | CloudUnit::Sessions => format::count(v),
        };
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let (worst_word, worst) = if series.floor {
            (t!("cli.cloud.min"), values.iter().copied().fold(f64::INFINITY, f64::min))
        } else {
            (t!("cli.cloud.max"), values.iter().copied().fold(f64::NEG_INFINITY, f64::max))
        };
        let limit = series.limit_label.as_deref().map(|l| format!(" · {l}")).unwrap_or_default();
        println!(
            "  {:<24} {} {:<10} {worst_word} {}{}",
            series.label,
            t!("cli.cloud.avg"),
            show(mean),
            show(worst),
            out.dim(&limit)
        );
    }
    for note in &s.notes {
        println!("{}", out.dim(note));
    }
}
