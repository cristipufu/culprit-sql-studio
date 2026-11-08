//! `culprit waits`: where database time goes, by wait category with the queries behind each, plus the server's pressure
//! signals; `--now` samples what it waits on right now.

use std::path::PathBuf;

use clap::Args;
use culprit_core::{
    Error, FileIo, Pressure, Replica, ReplicaRole, Result, Signal, SignalScope, SignalStatus, SignalUnit, Snapshot,
    WaitSample, WaitsReport, format, t,
};
use culprit_engine::Workspace;
use serde_json::json;

use crate::output::{Format, Out};

#[derive(Args)]
pub struct WaitsArgs {
    /// A saved connection name.
    pub connection: Option<String>,
    /// Sample what the server waits on right now instead of reading the latest scan.
    #[arg(long)]
    pub now: bool,
    /// Length of the --now sample in seconds (1 to 60).
    #[arg(long, default_value_t = 30)]
    pub seconds: u32,
    /// Read the latest scan with this window (SQL Server Query Store days).
    #[arg(long)]
    pub window_days: Option<u32>,
    /// Read the latest scan with this window, in days, hours or minutes: 6h, 90m, 2d. Instead of --window-days.
    #[arg(long, value_parser = crate::parse_window_arg, conflicts_with = "window_days", value_name = "DURATION")]
    pub window: Option<u32>,
    /// Explain a saved snapshot or report file instead of a connection's latest scan.
    #[arg(long, conflicts_with = "now")]
    pub file: Option<PathBuf>,
}

pub async fn run(ws: &Workspace, args: WaitsArgs, out: &Out) -> Result<u8> {
    if args.now {
        let connection = args.connection.ok_or_else(|| Error::invalid(t!("cli.waits.name_one")))?;
        let seconds = args.seconds.clamp(1, 60);
        if out.format() == Format::Text {
            eprintln!("{}", t!("cli.waits.sampling", connection = &connection, seconds = seconds.to_string()));
        }
        let sample = ws.wait_sample(&connection, seconds).await?;
        match out.format() {
            Format::Json => println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "connection": connection, "sample": sample }))
                    .unwrap_or_default()
            ),
            _ => print_sample(&connection, &sample),
        }
        return Ok(0);
    }
    let (name, snapshot) = match (&args.file, &args.connection) {
        (Some(path), _) => {
            let snapshot = ws.analyze_file(path)?.snapshot;
            (snapshot.connection.clone(), snapshot)
        }
        (None, Some(connection)) => {
            let report = ws
                .latest_report(connection, crate::window_minutes(args.window, args.window_days))?
                .ok_or_else(|| {
                    Error::not_found(t!("engine.reporting.no_report_yet", name = connection))
                        .with_hint(t!("engine.reporting.no_report_yet_hint"))
                })?;
            (connection.clone(), report.snapshot)
        }
        (None, None) => return Err(Error::invalid(t!("cli.waits.name_or_file"))),
    };
    let waits = culprit_engine::explain_waits(&snapshot);
    match out.format() {
        Format::Json => {
            let mut value = json!({ "connection": name, "waits": waits, "pressure": snapshot.pressure, "operations": snapshot.operations.counters_only() });
            if let Some(resources) = &snapshot.operations.node_resources {
                value["node_resources"] = json!(resources);
            }
            if let Some(history) = &snapshot.operations.termination_history {
                value["terminations"] = json!(history);
            }
            if let Some(reason) = snapshot.source_problem(&[
                culprit_core::source_id::DOCDB_ACTIVITY_HISTORY,
                culprit_core::source_id::DOCDB_TERMINATIONS,
                culprit_core::source_id::DOCDB_NODE_RESOURCES,
            ]) {
                value["notes"] = json!([reason]);
            }
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        }
        _ => print_report(&name, &snapshot, waits.as_ref()),
    }
    Ok(0)
}

fn bar(share: f64) -> String {
    let filled = (share.clamp(0.0, 1.0) * 20.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(20 - filled))
}

fn print_report(name: &str, snapshot: &Snapshot, waits: Option<&WaitsReport>) {
    match waits {
        Some(w) => {
            let title = if w.activity.is_some() {
                t!("cli.waits.activity_title", name = name)
            } else {
                t!("cli.waits.title", name = name)
            };
            println!("{title} · {}", w.source);
            println!();
            if let Some(activity) = &w.activity {
                let observations = t!(
                    "cli.waits.observations",
                    retained_n = activity.period.observations,
                    retained = format::count(activity.period.observations),
                    weighted_n = activity.period.weighted_observations,
                    weighted = format::count(activity.period.weighted_observations)
                );
                let partial = if activity.period.partial { t!("cli.waits.partial") } else { String::new() };
                println!("{observations} {partial}");
                for c in &activity.categories {
                    println!(
                        "{:>5}  {}  {:<22} {}",
                        format::percent(c.share),
                        bar(c.share),
                        c.label,
                        t!("cli.waits.weight", weight = format::count(c.weighted_observations))
                    );
                    for e in c.events.iter().take(5) {
                        let node = e.node.clone().unwrap_or_else(|| t!("cli.unknown"));
                        println!(
                            "{:>29}{} · {} · {}{} · {}",
                            "",
                            e.component,
                            e.wait_event,
                            t!("cli.waits.node", node = node),
                            e.query_id
                                .as_ref()
                                .map(|id| format!(" · {}", t!("cli.waits.query", id = id)))
                                .unwrap_or_default(),
                            t!("cli.waits.weight", weight = format::count(e.weighted_observations))
                        );
                    }
                    for q in c.queries.iter().take(3) {
                        let share =
                            t!("cli.waits.share_of_weight", share = format::percent(q.share_of_category), id = &q.id);
                        println!("{:>29}{share} {}", "", q.text.as_deref().map(|s| short(s, 70)).unwrap_or_default());
                    }
                }
            }
            for c in &w.categories {
                println!(
                    "{:>5}  {}  {:<22} {:>9}   {}",
                    format::percent(c.share),
                    bar(c.share),
                    c.label,
                    format::duration(c.wait_ms),
                    c.top_wait.as_deref().unwrap_or("")
                );
                if let Some(detail) = &c.detail {
                    println!("{:>29}{detail}", "");
                }
                for q in c.queries.iter().take(3) {
                    let share = t!("cli.waits.of_it", share = format!("{:>5}", format::percent(q.share_of_category)));
                    let query = t!("cli.waits.query", id = format!("{:<8}", q.id));
                    println!("{:>29}{share}  {query} {}", "", short(&q.text, 70));
                }
            }
            for note in &w.notes {
                println!();
                println!("{}", t!("cli.note", note = note));
            }
        }
        None => {
            println!("{}", t!("cli.waits.none", name = name));
            if let Some(reason) = snapshot.source_problem(&[culprit_core::source_id::DOCDB_ACTIVITY_HISTORY]) {
                println!("{}", t!("cli.waits.activity_unavailable", reason = reason));
            }
            println!("{}", t!("cli.waits.sample_now", command = format!("culprit waits {name} --now")));
        }
    }
    if let Some(history) = &snapshot.operations.termination_history {
        let n = history.events.len();
        println!("\n{}", t!("cli.waits.terminations", source = &history.source, n = n, count = n.to_string()));
        println!(
            "{} {}. {}",
            t!("cli.waits.requested", from = &history.requested_start, to = &history.requested_end),
            history.visibility,
            if history.partial { t!("cli.waits.partial") } else { String::new() }
        );
        for event in history.events.iter().take(12) {
            let node = event.node.clone().unwrap_or_else(|| t!("cli.unknown"));
            let query = event.query_id.clone().unwrap_or_else(|| t!("cli.waits.unresolved"));
            println!(
                "  {} · {} · {} · {}",
                event.ended_at,
                event.reason.label(),
                t!("cli.waits.node", node = node),
                t!("cli.waits.query", id = query)
            );
        }
        if history.events.len() > 12 {
            println!("  {}", t!("cli.waits.first_records"));
        }
        for note in &history.notes {
            println!("  {note}");
        }
    } else if let Some(reason) = snapshot.source_problem(&[culprit_core::source_id::DOCDB_TERMINATIONS]) {
        println!("{}", t!("cli.waits.terminations_unavailable", reason = reason));
    }
    if let Some(p) = &snapshot.pressure {
        print_signals(&t!("cli.waits.health"), p, &p.signals);
        print_storage(&p.files);
        print_replicas(&p.replicas);
    }
    if let Some(resources) = &snapshot.operations.node_resources {
        println!(
            "\n{}",
            t!(
                "cli.waits.node_resources",
                source = &resources.source,
                from = &resources.observed_from,
                to = &resources.observed_until
            )
        );
        let limit = |limit: Option<culprit_core::MemoryLimit>| match limit {
            Some(culprit_core::MemoryLimit::Unlimited) => t!("cli.waits.unlimited"),
            Some(culprit_core::MemoryLimit::Bytes(n)) => format::bytes(n),
            None => t!("cli.unknown"),
        };
        for node in resources
            .nodes
            .iter()
            .filter(|n| !n.available)
            .chain(resources.nodes.iter().filter(|n| n.available))
            .take(12)
        {
            if !node.available {
                println!("  {}: {}", node.node, t!("cli.waits.node_unavailable"));
                continue;
            }
            let unknown = || t!("cli.unknown");
            let host = t!(
                "cli.waits.host",
                user = node.cpu_user_fraction.map_or_else(unknown, format::percent),
                system = node.cpu_system_fraction.map_or_else(unknown, format::percent),
                available = node.host_memory_available_bytes.map_or_else(unknown, format::bytes),
                total = node.host_memory_total_bytes.map_or_else(unknown, format::bytes)
            );
            println!("  {}: {host}", node.node);
            if let Some(memory) = &node.process_memory {
                let tracked = t!(
                    "cli.waits.process_memory",
                    tracked = memory.tracked_bytes.map_or_else(unknown, format::bytes),
                    soft = limit(memory.soft_limit),
                    hard = limit(memory.hard_limit)
                );
                println!("    {}: {tracked}", memory.name);
            }
        }
        if resources.nodes.len() > 12 {
            println!("  {}", t!("cli.waits.first_nodes"));
        }
        if resources.partial {
            println!("  {}", t!("cli.waits.resources_partial"));
        }
        for note in &resources.notes {
            println!("  {note}");
        }
    } else if let Some(reason) = snapshot.source_problem(&[culprit_core::source_id::DOCDB_NODE_RESOURCES]) {
        println!("{}", t!("cli.waits.resources_unavailable", reason = reason));
    }
    let o = &snapshot.operations;
    for space in &o.spaces {
        let growth = match space.growth_enabled {
            Some(true) => t!("cli.waits.growth.configured"),
            Some(false) => t!("cli.waits.growth.unavailable"),
            None => t!("cli.unknown"),
        };
        let space_line = t!(
            "cli.waits.space",
            used = format::bytes(space.used_bytes),
            total = format::bytes(space.total_bytes),
            scope = scope_label(space.scope),
            growth = growth
        );
        println!("{}: {space_line} {}", space.name, t!("cli.waits.headroom"));
        if let Some(reason) = &space.reuse_wait {
            println!("  {}", t!("cli.waits.log_reuse", reason = reason));
        }
        for part in &space.parts {
            println!("  {}: {}", part.name, format::bytes(part.bytes));
        }
    }
    for c in &o.counters {
        let since = c.since.clone().unwrap_or_else(|| t!("cli.waits.unknown_reset"));
        let recorded = t!(
            "cli.waits.counter",
            n = c.count,
            count = format::count(c.count),
            scope = scope_label(c.scope),
            since = since
        );
        println!(
            "{} · {}: {recorded} {}",
            culprit_core::wire_name(&c.kind).replace('-', " "),
            c.subject,
            t!("cli.waits.counter_not_incident")
        );
    }
    if let Some(wal) = &o.sqlite {
        let size = wal.wal_bytes.map_or_else(|| t!("cli.unknown"), format::bytes);
        println!("{} {}", t!("cli.waits.wal", size = size), t!("cli.waits.wal_not_work"));
    }
    for seq in &o.sequences {
        let line = t!(
            "cli.waits.sequence",
            current = seq.current.to_string(),
            minimum = seq.minimum.to_string(),
            maximum = seq.maximum.to_string(),
            increment = seq.increment.to_string(),
            cycles = seq.cycles.to_string()
        );
        println!("{}: {line} {}", seq.name, t!("cli.waits.sequence_no_forecast"));
    }
    for age in &o.multixacts {
        println!("{}: {}", age.subject, t!("cli.waits.multixact", age = format::count(age.age)));
    }
    if let Some(cluster) = &o.cluster {
        let state = cluster.local_state.clone().unwrap_or_else(|| t!("cli.waits.state_unknown"));
        println!(
            "{}",
            t!(
                "cli.waits.galera",
                ready = flag_label(cluster.ready),
                connected = flag_label(cluster.connected),
                primary = flag_label(cluster.primary_component),
                state = state
            )
        );
    }
}

fn scope_label(scope: culprit_core::FileScope) -> String {
    match scope {
        culprit_core::FileScope::Database => t!("cli.waits.scope.database"),
        culprit_core::FileScope::Server => t!("cli.waits.scope.server"),
    }
}
fn flag_label(value: Option<bool>) -> String {
    match value {
        Some(true) => t!("cli.yes"),
        Some(false) => t!("cli.no"),
        None => t!("cli.unknown"),
    }
}

fn print_sample(name: &str, s: &WaitSample) {
    let title = t!("cli.waits.now_title", name = name, seconds = s.seconds.round().to_string());
    println!("{title} · {}", s.method);
    println!();
    if s.categories.is_empty() {
        println!("{}", t!("cli.waits.nothing_waited"));
    }
    for c in &s.categories {
        let share = if s.total_ms > 0.0 { c.wait_ms / s.total_ms } else { 0.0 };
        let (label, _) = culprit_core::waits::category_info(culprit_core::waits::category_key(&c.category), s.engine);
        println!(
            "{:>5}  {}  {:<22} {:>9}   {}",
            format::percent(share),
            bar(share),
            label,
            format::duration(c.wait_ms),
            c.wait_type.as_deref().unwrap_or("")
        );
    }
    if let Some(a) = s.active_sessions {
        println!();
        println!("{}", t!("cli.waits.busy_sessions", count = format::count(a)));
    }
    if !s.signals.is_empty() {
        print_signals(&t!("cli.waits.during_sample"), &Pressure::default(), &s.signals);
    }
    for note in &s.notes {
        println!("{}", t!("cli.note", note = note));
    }
}

fn print_signals(title: &str, p: &Pressure, signals: &[Signal]) {
    if signals.is_empty() {
        return;
    }
    println!();
    match p.uptime_hours {
        Some(h) => {
            println!("{}", t!("cli.waits.signals_since", title = title, span = format::duration(h * 3_600_000.0)))
        }
        None => println!("{title}"),
    }
    for s in signals {
        let mark = match s.status {
            SignalStatus::Bad => "●",
            SignalStatus::Watch => "◐",
            SignalStatus::Good => "○",
            SignalStatus::Info => " ",
        };
        let scope = match s.scope {
            SignalScope::Now => t!("cli.waits.signal_scope.now"),
            SignalScope::SinceStartup => t!("cli.waits.signal_scope.since_start"),
            SignalScope::Sampled => t!("cli.waits.signal_scope.sampled"),
            SignalScope::LastHour => t!("cli.waits.signal_scope.last_hour"),
            SignalScope::QueryHistory => t!("cli.waits.signal_scope.query_history"),
            SignalScope::Window => t!("cli.waits.signal_scope.window"),
        };
        println!("  {mark} {:<36} {:>10}  {:<11} {}", s.label, value(s), scope, s.healthy.as_deref().unwrap_or(""));
    }
}

pub fn value(s: &Signal) -> String {
    match s.unit {
        SignalUnit::Percent => format::percent_value(s.value),
        // Under one every ten seconds, a per-minute rate says more than "0.0/s".
        SignalUnit::PerSecond if s.value > 0.0 && s.value < 0.1 => {
            t!("cli.unit.per_minute", n = format::count(s.value * 60.0))
        }
        SignalUnit::PerSecond => t!("cli.unit.per_second", n = format::count(s.value)),
        SignalUnit::PerHour => t!("cli.unit.per_hour", n = format::count(s.value)),
        SignalUnit::Count => format::count(s.value),
        SignalUnit::Bytes => format::bytes(s.value),
        SignalUnit::Milliseconds => format::duration(s.value),
        SignalUnit::Seconds => format::duration(s.value * 1000.0),
    }
}

/// Latency per file, slowest first: what reads and writes took on average since startup.
fn print_storage(files: &[FileIo]) {
    if files.is_empty() {
        return;
    }
    println!();
    println!("{}", t!("cli.waits.storage"));
    let avg = |ms: Option<f64>, n: f64| match ms {
        Some(ms) if n > 0.0 => format!("{:>9} × {:<6}", latency(ms / n), format::count(n)),
        _ if n > 0.0 => format!("{:>9} × {:<6}", t!("cli.waits.untimed"), format::count(n)),
        _ => format!("{:>18}", ""),
    };
    for f in files.iter().take(12) {
        let name = match &f.database {
            Some(db) if !f.name.starts_with(db.as_str()) => format!("{db} · {}", f.name),
            _ => f.name.clone(),
        };
        println!(
            "  {:<34} {}  {}",
            short(&name, 34),
            t!("cli.waits.reads", reads = avg(f.read_ms, f.reads)),
            t!("cli.waits.writes", writes = avg(f.write_ms, f.writes))
        );
    }
}

/// Storage latency reads best in microseconds below a tenth of a millisecond (cache hits, fast SSDs).
fn latency(ms: f64) -> String {
    if ms < 0.1 { t!("common.duration.us", n = format!("{:.0}", ms * 1000.0)) } else { format::duration(ms) }
}

fn print_replicas(replicas: &[Replica]) {
    if replicas.is_empty() {
        return;
    }
    println!();
    println!("{}", t!("cli.waits.replication"));
    for r in replicas {
        let role = match r.role {
            ReplicaRole::Replica => t!("cli.waits.role.replica"),
            ReplicaRole::ThisServer => t!("cli.waits.role.this_server"),
            ReplicaRole::Slot => t!("cli.waits.role.slot"),
        };
        let mut parts: Vec<String> = Vec::new();
        parts.extend(r.state_words());
        parts.extend(r.sync.clone());
        if let Some(s) = r.lag_seconds {
            parts.push(t!("cli.waits.behind", duration = format::duration(s * 1000.0)));
        }
        if let Some(b) = r.lag_bytes {
            parts.push(if r.role == ReplicaRole::Slot {
                t!("cli.waits.kept", size = format::bytes(b))
            } else {
                t!("cli.waits.not_applied", size = format::bytes(b))
            });
        }
        parts.extend(r.error.clone());
        println!("  {:<11} {:<28} {}", role, short(&r.name, 28), parts.join(" · "));
    }
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max - 1).collect::<String>())
}
