//! The per-database dashboard, composed from one report: a takeaway, the numbers behind it and the rows to open for
//! each of fourteen tiles. `now` (sessions right now) comes from a live read and is added with [`with_now`].
//!
//! Every tile says something on every engine: when an engine, login or scan can't show a signal, the tile is
//! `unavailable` and its takeaway says why, in the engine's own terms.

use std::collections::{BTreeMap, HashMap, HashSet};

use culprit_core::signals::{FileIo, FileKind, Replica, ReplicaRole};
use culprit_core::{
    Capability, Dashboard, Engine, Finding, ItemKind, LiveState, QueryStat, Report, ServerInfo, Severity, Signal,
    SignalStatus, SignalUnit, Snapshot, Tile, TileItem, TileKey, TileMetric, TileStatus, WaitScope, Workload, format,
    t,
};

use crate::support::{app_queries, query_handle, statement_handle, top_findings_share, window_phrase};

mod database_time;
mod deadlocks;
mod findings;
mod health;
mod indexes;
mod plans;
mod right_now;
mod signal_tiles;
#[cfg(test)]
mod tests;
mod top_queries;
mod waits;

use database_time::*;
use deadlocks::*;
use findings::*;
use health::*;
use indexes::*;
use plans::*;
pub use right_now::*;
use signal_tiles::*;
use top_queries::*;
use waits::*;

/// Below this many hours, per-day figures would be forecasts from a sliver of data (the UI's `PER_DAY_MIN_HOURS`).
const PER_DAY_MIN_HOURS: f64 = 20.0;

/// Most urgent first when two tiles share a status.
const PRIORITY: [TileKey; 14] = [
    TileKey::Now,
    TileKey::DbTime,
    TileKey::Findings,
    TileKey::Statistics,
    TileKey::Failures,
    TileKey::Replication,
    TileKey::Deadlocks,
    TileKey::Storage,
    TileKey::Health,
    TileKey::Connections,
    TileKey::Waits,
    TileKey::Plans,
    TileKey::Indexes,
    TileKey::TopQueries,
];

/// The dashboard for one report. The `now` tile says sessions weren't read until [`with_now`] adds a live read.
pub fn dashboard(report: &Report) -> Dashboard {
    let s = &report.snapshot;
    let tiles = vec![
        db_time(s),
        findings(report),
        top_queries(s),
        waits(s),
        not_read(s),
        health(report),
        deadlocks(report),
        indexes(report),
        plans(s),
        storage(report),
        connections(report),
        replication(report),
        failures(report),
        statistics(report),
    ];
    let mut d = Dashboard {
        connection: s.connection.clone(),
        engine: s.server.engine,
        product: s.server.product.clone(),
        database: s.server.database.clone(),
        environment: s.environment,
        report_id: s.id.clone(),
        taken_at: s.taken_at.clone(),
        coverage: s
            .workload
            .as_ref()
            .map(|w| t!("view.dashboard.coverage", source = w.source_name(), window = window_phrase(w))),
        headline: String::new(),
        attention: Vec::new(),
        tiles,
    };
    summarize(&mut d);
    d
}

/// Put a live read into the dashboard and re-rank what needs a look.
pub fn with_now(d: &mut Dashboard, live: &LiveState) {
    let tile = now(live);
    match d.tiles.iter_mut().find(|t| t.key == TileKey::Now) {
        Some(slot) => *slot = tile,
        None => d.tiles.push(tile),
    }
    summarize(d);
}

/// Sessions couldn't be read: the tile says why and the rest of the dashboard stands.
pub fn without_now(d: &mut Dashboard, reason: &str) {
    let mut tile =
        Tile::new(TileKey::Now, TileStatus::Unavailable, t!("view.dashboard.now.unreadable", reason = reason));
    tile.detail = Some(t!("view.dashboard.now.unreadable_detail"));
    match d.tiles.iter_mut().find(|t| t.key == TileKey::Now) {
        Some(slot) => *slot = tile,
        None => d.tiles.push(tile),
    }
    summarize(d);
}

/// One shared projection for desktop polling, CLI reads and offline/demo consumers.
pub fn refresh(mut dashboard: Dashboard, result: culprit_core::Result<LiveState>) -> culprit_core::DashboardLive {
    let (live, error) = match result {
        Ok(live) => {
            with_now(&mut dashboard, &live);
            (Some(live), None)
        }
        Err(error) => {
            without_now(&mut dashboard, &error.message);
            (None, Some(error))
        }
    };
    culprit_core::DashboardLive { dashboard, live, error }
}

fn summarize(d: &mut Dashboard) {
    // Without query statistics, turning them on comes first: only a problem happening right now is more urgent.
    let no_workload = d.tiles.iter().any(|t| t.key == TileKey::DbTime && t.status == TileStatus::Unavailable);
    let rank = |k: TileKey| match k {
        TileKey::Statistics if no_workload => 1,
        TileKey::Now => 0,
        _ => PRIORITY.iter().position(|p| *p == k).unwrap_or(PRIORITY.len()) + 1,
    };
    let mut attention: Vec<&Tile> =
        d.tiles.iter().filter(|t| matches!(t.status, TileStatus::Bad | TileStatus::Watch)).collect();
    attention.sort_by(|a, b| b.status.cmp(&a.status).then(rank(a.key).cmp(&rank(b.key))));
    d.attention = attention.iter().map(|t| t.key).collect();
    d.headline = match attention.first() {
        Some(t) => t.takeaway.clone(),
        None => {
            let missing = d.tiles.iter().filter(|t| t.status == TileStatus::Unavailable).count();
            match missing {
                0 => t!("view.dashboard.headline.nothing"),
                n => t!("view.dashboard.headline.nothing_unavailable", n = n, count = n.to_string()),
            }
        }
    };
}

/// "SQL Server", "PostgreSQL", "MySQL" or "MariaDB": the name people know the server by.
pub fn family(server: &ServerInfo) -> &'static str {
    server.name()
}

/// A stretch of time as the dashboard says it: whole days from 47 hours, whole hours from an hour and a half, minutes
/// (at least one) below that.
enum Span {
    Days(f64),
    Hours(f64),
    Minutes(f64),
}

fn span_of(hours: f64) -> Span {
    if hours >= 47.0 {
        Span::Days((hours / 24.0).round())
    } else if hours >= 1.5 {
        Span::Hours(hours.round())
    } else {
        Span::Minutes((hours * 60.0).round().max(1.0))
    }
}

/// "the last day", "the last 6 hours".
fn last(hours: f64) -> String {
    if (hours - 24.0).abs() < 0.5 {
        return t!("view.dashboard.last.day");
    }
    match span_of(hours) {
        Span::Days(n) => t!("view.dashboard.last.days", n = n, count = n.to_string()),
        Span::Hours(n) => t!("view.dashboard.last.hours", n = n, count = n.to_string()),
        Span::Minutes(n) => t!("view.dashboard.last.minutes", n = n, count = n.to_string()),
    }
}

/// "2 days", "7 hours", "1 minute".
fn span(hours: f64) -> String {
    match span_of(hours) {
        Span::Days(n) => t!("view.dashboard.span.days", n = n, count = n.to_string()),
        Span::Hours(n) => t!("view.dashboard.span.hours", n = n, count = n.to_string()),
        Span::Minutes(n) => t!("view.dashboard.span.minutes", n = n, count = n.to_string()),
    }
}

/// A tile with nothing to show without query history: `takeaway` says what that leaves out.
fn no_workload(key: TileKey, takeaway: String) -> Tile {
    let mut t = Tile::new(key, TileStatus::Unavailable, takeaway);
    t.detail =
        Some(t!("view.dashboard.no_workload.detail", statistics = culprit_core::t!("core.dashboard.tile.statistics")));
    t
}

fn severity_status(s: Severity) -> TileStatus {
    match s {
        Severity::High => TileStatus::Bad,
        Severity::Medium => TileStatus::Watch,
        Severity::Low => TileStatus::Info,
    }
}

fn missing_reason(s: &Snapshot, capability: Capability) -> Option<&culprit_core::MissingCapability> {
    s.server.missing.iter().find(|m| m.capability == capability)
}

fn setting<'a>(s: &'a Snapshot, name: &str) -> Option<&'a str> {
    s.settings.iter().find(|x| x.name.eq_ignore_ascii_case(name)).map(|x| x.value.as_str())
}

fn is_off(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"))
}

/// A signal's value as a person reads it, with its unit.
pub fn signal_value(s: &Signal) -> String {
    match s.unit {
        SignalUnit::Percent => format::percent_value(s.value),
        SignalUnit::PerSecond => t!("view.dashboard.signal.per_second", value = format::count(s.value)),
        SignalUnit::PerHour => t!("view.dashboard.signal.per_hour", value = format::count(s.value)),
        SignalUnit::Count => format::count(s.value),
        SignalUnit::Bytes => format::bytes(s.value),
        SignalUnit::Milliseconds => format::duration(s.value),
        SignalUnit::Seconds => format::duration(s.value * 1000.0),
    }
}

fn signal_status(s: SignalStatus) -> TileStatus {
    match s {
        SignalStatus::Bad => TileStatus::Bad,
        SignalStatus::Watch => TileStatus::Watch,
        SignalStatus::Good => TileStatus::Good,
        SignalStatus::Info => TileStatus::Info,
    }
}
