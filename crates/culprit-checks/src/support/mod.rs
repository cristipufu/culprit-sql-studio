mod handles;
mod predicates;
#[cfg(test)]
pub(crate) mod tests;

use culprit_core::i18n::{Message, MessageValue};
pub use handles::*;
pub use predicates::*;

use culprit_core::{
    Capability, Category, Confidence, Engine, Finding, Impact, QueryRef, QueryStat, Severity, Snapshot, Subject,
    SubjectKind, format, key, t,
};

pub fn finding(check: &str, key: &str, category: Category, subject: Subject) -> Finding {
    Finding {
        id: format!("{check}:{key}"),
        check: check.to_string(),
        category,
        severity: Severity::Low,
        actionability: Default::default(),
        confidence: Confidence::Medium,
        title: String::new(),
        summary: String::new(),
        subject,
        impact: Impact::default(),
        metrics: Vec::new(),
        queries: Vec::new(),
        fix: None,
        verify: Vec::new(),
        verification_basis: Default::default(),
        caveats: Vec::new(),
        last_seen: None,
        inactive: false,
        facts: Default::default(),
    }
}

pub fn subject(kind: SubjectKind, name: impl Into<String>) -> Subject {
    Subject { kind, name: name.into() }
}

/// Ranking score for findings measured in database time: √share keeps a 4% query above a 1% one without letting one giant query flatten everything else.
pub fn score_for_share(share: f64) -> f64 {
    (share.clamp(0.0, 1.0).sqrt() * 100.0).min(100.0)
}

pub fn severity_for_share(share: f64) -> Severity {
    if share >= 0.15 {
        return Severity::High;
    }
    if share >= 0.03 {
        return Severity::Medium;
    }
    Severity::Low
}

/// Database time a day a finding needs behind it for Medium, and for High: a share of a quiet database's time is still a
/// small number.
pub const MEDIUM_MS_A_DAY: f64 = 60_000.0;
pub const HIGH_MS_A_DAY: f64 = 600_000.0;

/// Severity for a finding from the database time behind it (`time_ms` in the workload's window, `share` of all
/// of it): the share says how much of the workload it is, the amount whether that's worth anyone's time. The amount is
/// the time a day over at least an hour of window; under an hour, the window's own total, which a day holds at least.
/// A window of unknown length (statistics never reset where the engine doesn't date them) says no amount a day: such
/// time is never High.
/// Where the source left time unattributed (MySQL's catch-all digest row once its table is full), shares are of the time
/// it could attribute: the grade takes the share of everything there may be, the lower bound.
pub fn severity_for_time(snapshot: &Snapshot, time_ms: f64, share: f64) -> Severity {
    severity_on_amount(snapshot, rate_per_day(snapshot, time_ms), time_ms, share)
}

/// [`severity_for_time`] for time spent by statements (`time_ms` of `queries`' time: theirs, their spills, their
/// waits), graded at the pace their runs came ([`rate_per_day_of`]): a burst on its total, never extrapolated to a day.
pub fn severity_for_time_of(snapshot: &Snapshot, queries: &[&QueryStat], time_ms: f64, share: f64) -> Severity {
    severity_on_amount(snapshot, rate_per_day_of(snapshot, queries, time_ms), time_ms, share)
}

fn severity_on_amount(snapshot: &Snapshot, a_day: Option<f64>, time_ms: f64, share: f64) -> Severity {
    let by_amount = match snapshot.workload.as_ref() {
        Some(w) if w.window_unknown() => Severity::Medium,
        Some(_) => severity_for_amount(a_day.unwrap_or(time_ms)),
        None => Severity::Low,
    };
    let share = match snapshot.workload.as_ref().and_then(|w| Some((w, w.untracked_time_ms?))) {
        Some((w, untracked)) if w.total_time_ms + untracked > 0.0 => share.min(time_ms / (w.total_time_ms + untracked)),
        _ => share,
    };
    severity_for_share(share).min(by_amount)
}

/// [`severity_for_time_of`] for a share of all the workload's database time spent by `queries`.
pub fn severity_for_share_of(snapshot: &Snapshot, queries: &[&QueryStat], share: f64) -> Severity {
    let total = snapshot.workload.as_ref().map_or(0.0, |w| w.total_time_ms);
    severity_for_time_of(snapshot, queries, share * total, share)
}

/// The grade of `queries`' whole impact ([`query_impact`]): over Culprit's scans where they measured them, the share
/// and the amount a day it is of what the database ran in those scans ([`severity_over_scans`]); otherwise their share
/// of the workload at the pace their runs came ([`severity_for_share_of`]).
pub fn severity_for_impact(snapshot: &Snapshot, queries: &[&QueryStat], impact: &QueryImpact) -> Severity {
    severity_for_statement_share(snapshot, queries, impact.share)
}

/// The grade of `share` of database time spent by `queries` (their share as [`query_impact`] states it, or a fraction
/// of it: what an index would save of their time), on one basis: over Culprit's scans where they measured them, the
/// share and the amount a day it is of what the database ran in those scans ([`severity_over_scans`]); otherwise at the
/// pace their runs came ([`severity_for_share_of`]). Never the scans' share with the time their totals hold.
pub fn severity_for_statement_share(snapshot: &Snapshot, queries: &[&QueryStat], share: f64) -> Severity {
    severity_over_scans(snapshot, queries, share).unwrap_or_else(|| severity_for_share_of(snapshot, queries, share))
}

/// The amount alone of [`severity_for_statement_share`]: what `share` of database time spent by `queries` comes to a
/// day, on the basis their share is on (Culprit's scans where they measured them, the workload's window otherwise),
/// graded as `severity_for_time` grades an amount. For a check that grades the share its own way.
pub fn amount_severity_of(snapshot: &Snapshot, queries: &[&QueryStat], share: f64) -> Severity {
    match scans_time_per_day(snapshot, queries) {
        Some(a_day) => severity_for_amount(a_day * share),
        None => {
            let total = snapshot.workload.as_ref().map_or(0.0, |w| w.total_time_ms);
            severity_for_time_of(snapshot, queries, share * total, 1.0)
        }
    }
}

/// What a figure kept only in a statement's totals is over, where it sits beside the statement's figures over Culprit's
/// scans (its share, runs and average, [`culprit_core::Workload::scans_of`]): "the 1277 days since statistics were
/// reset". A part of its time (its slow runs' excess, a spill, its waits), the rows or temp files a run handles: the
/// source keeps them only in its totals, so each figure's basis is named and the two aren't read as one period. `None`
/// where the statement's figures are the totals' (no scans measured it), or for a recent entry, whose totals are mostly
/// those scans' runs.
pub fn totals_basis(snapshot: &Snapshot, q: &QueryStat) -> Option<String> {
    let w = snapshot.workload.as_ref()?;
    w.scans_of(q).filter(|s| !s.is_recent(q))?;
    Some(w.window_phrase())
}

/// The hours [`severity_for_recent_time`] rates statement `q`'s recent time over: `hours`, or for runs that came in
/// bursts at least a day, so their total is never extrapolated to a day it didn't run.
pub fn burst_hours(snapshot: &Snapshot, q: &QueryStat, hours: f64) -> f64 {
    if ran_in_bursts(snapshot, q) { hours.max(24.0) } else { hours }
}

/// [`severity_for_time`] for time spent over the last `hours` of the window (since a plan took over, since a change):
/// the amount a day over those hours, or under an hour their own total, so a regression that began this morning isn't
/// diluted by the week before it.
pub fn severity_for_recent_time(hours: f64, time_ms: f64, share: f64) -> Severity {
    let a_day = if hours >= culprit_core::RATE_MIN_HOURS { time_ms * 24.0 / hours } else { time_ms };
    severity_for_share(share).min(severity_for_amount(a_day))
}

fn severity_for_amount(a_day_ms: f64) -> Severity {
    if a_day_ms >= HIGH_MS_A_DAY {
        Severity::High
    } else if a_day_ms >= MEDIUM_MS_A_DAY {
        Severity::Medium
    } else {
        Severity::Low
    }
}

pub fn base_score(severity: Severity) -> f64 {
    match severity {
        Severity::High => 45.0,
        Severity::Medium => 22.0,
        Severity::Low => 8.0,
    }
}

pub struct QueryImpact {
    pub share: f64,
    /// Database time a day, when the window is long enough to say so without extrapolating.
    pub per_day_ms: Option<f64>,
}

pub fn query_impact(snapshot: &Snapshot, queries: &[&QueryStat]) -> Option<QueryImpact> {
    let workload = snapshot.workload.as_ref()?;
    if queries.is_empty() {
        return None;
    }
    let total_ms: f64 = queries.iter().map(|q| q.total_time_ms).sum();
    // Each statement's share as every surface states it (`Workload::statement_share`): over Culprit's scans where they
    // measured it, otherwise weighed as the source allows (a plan cached for a month against one compiled an hour ago).
    let share: f64 = queries.iter().map(|q| workload.statement_share(q)).sum();
    let per_day_ms = if queries.iter().any(|q| workload.scans_of(q).is_some()) {
        // What the scans measured a day, beside the others' a day over the window.
        let days: Vec<f64> = queries.iter().filter_map(|q| workload.statement_time_per_day(q)).collect();
        (!days.is_empty()).then(|| days.iter().sum())
    } else {
        // A day's worth only of runs that came that way: a burst's or a single run's time spread over the window is a
        // rate it never ran at.
        workload.per_day_measured(total_ms).filter(|_| queries.iter().any(|q| workload.runs_per_day_of(q).is_some()))
    };
    Some(QueryImpact { share: share.min(1.0), per_day_ms })
}

/// The database's time a day over Culprit's scans, where they measured one of `queries` ([`culprit_core::Workload::scans_of`]): what
/// a share of database time over them is a share of. `None` where none of them is read by the scans.
pub fn scans_time_per_day(snapshot: &Snapshot, queries: &[&QueryStat]) -> Option<f64> {
    let w = snapshot.workload.as_ref()?;
    queries.iter().find_map(|q| w.scans_of(q)).map(|s| s.per_day(s.workload_time_ms))
}

/// The grade of a share of database time over Culprit's scans ([`scans_time_per_day`]): the share, and the amount a
/// day it is of what the database ran in them. `None` where none of `queries` is read by the scans.
pub fn severity_over_scans(snapshot: &Snapshot, queries: &[&QueryStat], share: f64) -> Option<Severity> {
    let a_day = scans_time_per_day(snapshot, queries)? * share;
    Some(severity_for_share(share).min(severity_for_amount(a_day)))
}

/// Measured workload supporting a query-driven index change, shared by checks and the advisor. Reuse the
/// existing amount bar and plan materiality bar: fast reads can still matter through sustained aggregate cost.
/// Schema obligations such as foreign keys have separate evidence and do not use this gate.
pub(crate) fn index_work_is_material(snapshot: &Snapshot, queries: &[&QueryStat]) -> bool {
    let current: Vec<_> = queries.iter().copied().filter(|q| !crate::query_is_history(snapshot, q)).collect();
    let Some(w) = &snapshot.workload else { return false };
    let Some(impact) = query_impact(snapshot, &current) else { return false };
    amount_severity_of(snapshot, &current, impact.share) >= Severity::Medium
        || current.iter().any(|q| q.executions > 0.0 && w.statement_mean_ms(q) >= culprit_core::insights::MATERIAL_MS)
}

/// Absolute query cost and its period on the same basis as the share. Mixed scan intervals cannot honestly be
/// added into a single measured total; in that case retain the individual evidence rather than inventing a period.
pub(crate) fn query_cost_context(snapshot: &Snapshot, queries: &[&QueryStat]) -> Option<String> {
    query_cost_message(snapshot, queries).map(|message| message.render())
}

pub(crate) fn query_cost_message(snapshot: &Snapshot, queries: &[&QueryStat]) -> Option<Message> {
    let w = snapshot.workload.as_ref()?;
    let impact = query_impact(snapshot, queries)?;
    // What the time is of: all database time, or only the runs the slow query log kept.
    let of = if crate::queries::slow_log_only(snapshot) { "slow_log" } else { "database" };
    let said = |time: f64, span: Message| {
        culprit_core::message!(
            "support.cost.time_over",
            time = MessageValue::Duration(time),
            of = of,
            span = span,
            share = MessageValue::Percent(impact.share)
        )
    };
    if queries.iter().any(|q| w.scans_of(q).is_some()) {
        if queries.len() != 1 {
            return None;
        }
        let scans = w.scans_of(queries[0])?;
        Some(said(scans.time_ms, scans.phrase_message()))
    } else {
        // Plan-cache shares can weight entries with different ages. Their raw totals are not an amount over one
        // common interval, so keep their existing per-entry evidence rather than manufacture a matching total.
        if w.hourly_time_ms.is_some() {
            return None;
        }
        Some(said(queries.iter().map(|q| q.total_time_ms).sum(), w.window_message()))
    }
}

pub fn apply_query_impact(f: &mut Finding, impact: &QueryImpact) {
    f.impact.db_time_share = Some(impact.share);
    f.impact.time_ms_per_day = impact.per_day_ms;
    f.impact.score = score_for_share(impact.share);
    f.impact.set_keyed_label(key!("support.impact.share"), share_label(impact.share));
}

pub fn share_label(share: f64) -> String {
    t!("support.impact.share", share = format::percent(share))
}

pub fn query_refs(queries: &[&QueryStat]) -> Vec<QueryRef> {
    queries.iter().map(|q| QueryRef { id: q.id.clone(), plan_node: None, note: None }).collect()
}

/// Text for a `--` comment line of fix SQL. A database's names can hold line breaks (a quoted identifier may), and one
/// would end the comment and turn the rest of the name into SQL that whoever runs the fix executes: every control
/// character and line or paragraph separator becomes a space. Every name or value a comment line carries goes through it.
pub fn comment_text(text: &str) -> String {
    text.chars().map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }).collect()
}

/// The engine's quoting (`Engine::dialect`).
pub fn quote(engine: Engine, ident: &str) -> String {
    engine.dialect().quote(ident)
}

pub fn qualified(engine: Engine, schema: &str, name: &str) -> String {
    engine.dialect().qualified(&[schema, name])
}

pub fn index_name(table: &str, columns: &[String]) -> String {
    let mut name = format!("IX_{table}");
    for c in columns {
        name.push('_');
        name.push_str(&c.replace(' ', ""));
    }
    name.chars().filter(|c| c.is_alphanumeric() || *c == '_').take(60).collect()
}

pub fn supports_online_index(snapshot: &Snapshot) -> bool {
    // Azure SQL runs every edition's features; RDS runs the edition it was bought with, read below.
    if snapshot.server.platform().is_some_and(|p| p.is_azure()) {
        return true;
    }
    snapshot
        .server
        .edition
        .as_deref()
        .is_some_and(|e| e.contains("Enterprise") || e.contains("Developer") || e.contains("Azure"))
}

/// [`create_index_sql`] for an index that holds only the rows `filter` keeps (SQL Server, written as it keeps filters:
/// `([is_deleted]=(0))`).
pub fn create_filtered_index_sql(
    snapshot: &Snapshot,
    schema: &str,
    table: &str,
    keys: &[String],
    include: &[String],
    filter: &str,
) -> String {
    index_sql(snapshot, schema, table, keys, include, Some(filter))
}

/// A new index on `schema.table` in the engine's dialect, online where the server's edition can build it so.
pub fn create_index_sql(snapshot: &Snapshot, schema: &str, table: &str, keys: &[String], include: &[String]) -> String {
    index_sql(snapshot, schema, table, keys, include, None)
}

fn index_sql(
    snapshot: &Snapshot,
    schema: &str,
    table: &str,
    keys: &[String],
    include: &[String],
    filter: Option<&str>,
) -> String {
    let engine = snapshot.server.engine;
    let kind = snapshot.table(schema, table).and_then(|t| t.kind.as_deref());
    let dialect = engine.spec().table_dialect(kind);
    let name = index_name(table, keys);
    let name = if engine == Engine::Mssql { name } else { name.to_ascii_lowercase() };
    let online = if supports_online_index(snapshot) { culprit_core::Online::Yes } else { culprit_core::Online::No };
    dialect.create_index(&culprit_core::NewIndex {
        target: &dialect.qualified(&[schema, table]),
        unique: false,
        name: &name,
        keys,
        include,
        filter,
        online,
    })
}

pub fn drop_index_for_snapshot(snapshot: &Snapshot, schema: &str, table: &str, index: &str) -> String {
    let kind = snapshot.table(schema, table).and_then(|t| t.kind.as_deref());
    snapshot.server.engine.spec().table_dialect(kind).drop_index(schema, table, index)
}

/// Days the cumulative counters (index usage, table stats) have been accumulating. One source for every window that
/// says so: the counters' own age as the scan measured it, else the server's start time where counters start with the
/// server. Not on PostgreSQL: its counters outlive a clean restart and are discarded by a crash recovery that keeps the
/// start time, so only the age its scan measured (from the last reset or discard) says how long they have counted.
pub fn counter_days(snapshot: &Snapshot) -> Option<f64> {
    if let Some(hours) = snapshot.pressure.as_ref().and_then(|p| p.uptime_hours) {
        return Some((hours / 24.0).max(0.0));
    }
    if matches!(snapshot.server.engine, Engine::Postgres | Engine::Yugabyte) {
        return None;
    }
    let started = snapshot.server.started_at.as_deref()?;
    let days = days_between(started, &snapshot.taken_at)?;
    Some(days.max(0.0))
}

/// Whether the counters cover enough time (20 hours) for a per-day rate that isn't an extrapolation.
pub fn per_day_is_measured(days: f64) -> bool {
    days * 24.0 >= culprit_core::PER_DAY_MIN_HOURS
}

/// "about 1.2K times a day" over 20 hours or more of counters, otherwise what was counted and over how long: "340 times
/// in the 5 hours since the server last restarted". `noun` is the key of a counted noun ([`counted`]).
pub fn how_many(snapshot: &Snapshot, total: f64, days: f64, noun: &str) -> String {
    // Counters that may have started long after the server did have no length to divide by: said as counted.
    if per_day_is_measured(days) && !counters_may_be_reset(snapshot) {
        let per_day = total / days;
        culprit_core::t!(
            "common.how_many.per_day",
            count = format::count(per_day),
            noun = counted(noun, per_day.round())
        )
    } else {
        culprit_core::t!(
            "common.how_many.since",
            count = format::count(total),
            noun = counted(noun, total),
            since = since_phrase(snapshot)
        )
    }
}

/// A counted noun for `n` of it, in the current language: `noun` is the key of a plural message, such as
/// `common.noun.time` (`{n, plural, one {time} other {times}}`).
pub fn counted(noun: &str, n: f64) -> String {
    culprit_core::i18n::t(noun, &[("n", culprit_core::i18n::Arg::Number(n))])
}

/// [`per_day_is_measured`] for counters of this snapshot: also never on counters that may have been reset since the
/// server started ([`counters_may_be_reset`]), whose length is unknown.
pub fn counters_measure_a_day(snapshot: &Snapshot, days: f64) -> bool {
    per_day_is_measured(days) && !counters_may_be_reset(snapshot)
}

/// Minutes performance_schema's statement digests may begin after the server started before its counters count as
/// possibly reset: the application's first statements come within moments of a start.
const PS_RESET_MINUTES: f64 = 10.0;

/// Whether MySQL's or MariaDB's performance_schema counters (table and index I/O, as well as statements) may have
/// started after the server did: its statement digests begin well after the start. They were truncated then (setup
/// scripts and `sys.ps_truncate_all_tables()` truncate the table and index counters with them), or no statement ran
/// before; either way the counters' length is unknown, so they are named "since the server started or its
/// performance_schema counters were last reset", with no span and no rate a day.
pub fn counters_may_be_reset(snapshot: &Snapshot) -> bool {
    let Some(w) =
        snapshot.workload.as_ref().filter(|w| w.kind() == Some(culprit_core::WorkloadKind::PerformanceSchema))
    else {
        return false;
    };
    let started = snapshot.server.started_at.as_deref().and_then(parse_epoch_seconds);
    let digests = w.window_start.as_deref().and_then(parse_epoch_seconds);
    snapshot.server.engine == Engine::Mysql
        && started.zip(digests).is_some_and(|(started, digests)| digests - started > PS_RESET_MINUTES * 60.0)
}

/// Days the usage counters have surely counted, for a claim of absence (an index never read, a table never deleted
/// from): their age ([`counter_days`]), or, where performance_schema's may have been reset since the server started
/// ([`counters_may_be_reset`]), the days its statement digests have counted: they start over together, so the server's
/// uptime, which a reset leaves as it was, says nothing about how long the counters have watched.
pub fn watched_days(snapshot: &Snapshot) -> Option<f64> {
    if snapshot.server.engine == Engine::Yugabyte {
        if !snapshot
            .source_outcome(culprit_core::source_id::DOCDB_INDEX_USAGE)
            .is_some_and(|o| matches!(o.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty))
        {
            return None;
        }
        // Each contributing node can reset independently. The latest proven reset bounds absence, but cannot be
        // used as the denominator of older positive totals from the other nodes.
        return snapshot
            .indexes
            .iter()
            .filter(|i| i.reads.is_some())
            .map(|i| days_between(i.counters_since.as_deref()?, &snapshot.taken_at))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .filter(|d| *d >= 0.0)
            .reduce(f64::min);
    }
    if counters_may_be_reset(snapshot) {
        return snapshot.workload.as_ref().map(|w| (w.measurement_hours() / 24.0).max(0.0));
    }
    counter_days(snapshot)
}

/// Whether the counters started at a statistics reset rather than at the last restart (PostgreSQL keeps statistics
/// across a clean restart, and `pg_stat_reset()` starts them over without one).
fn counters_since_reset(snapshot: &Snapshot) -> bool {
    if snapshot.server.engine == Engine::Yugabyte {
        return snapshot.pressure.as_ref().and_then(|p| p.uptime_hours).is_some();
    }
    let (Some(hours), Some(started)) =
        (snapshot.pressure.as_ref().and_then(|p| p.uptime_hours), snapshot.server.started_at.as_deref())
    else {
        return false;
    };
    let since_start = days_between(started, &snapshot.taken_at).map(|d| d * 24.0);
    snapshot.server.engine == Engine::Postgres && since_start.is_some_and(|h| (h - hours).abs() > 0.1)
}

pub fn days_between(from: &str, to: &str) -> Option<f64> {
    let a = parse_epoch_seconds(from)?;
    let b = parse_epoch_seconds(to)?;
    Some((b - a) / 86_400.0)
}

/// Seconds since 1970 for an ISO timestamp (`2026-10-14T08:54:29Z`, with or without an offset); None for anything
/// that isn't a real date and time. The one parser is `culprit_core::format::epoch_seconds`.
pub fn parse_epoch_seconds(text: &str) -> Option<f64> {
    culprit_core::format::epoch_seconds(text)
}

/// How long the server's counters have counted: since what, and for how long when that is known.
enum Counted {
    /// Only since what ("since the server last restarted"), with no span.
    Since(String),
    Days {
        days: String,
        since: String,
    },
    Hours {
        n: f64,
        since: String,
    },
    Minutes {
        n: f64,
        since: String,
    },
}

fn counted_since(snapshot: &Snapshot) -> Counted {
    if snapshot.server.engine == Engine::Yugabyte && counter_days(snapshot).is_none() {
        return Counted::Since(t!("support.since.cluster_nodes"));
    }
    // MariaDB's userstat counters run from when userstat was turned on (or flushed), which the server doesn't say.
    if snapshot.server.engine == Engine::Mysql
        && !snapshot.server.has(Capability::PerformanceSchema)
        && snapshot.server.has(Capability::IndexUsage)
    {
        return Counted::Since(t!("support.since.userstat"));
    }
    // Truncated with the statements (`sys.ps_truncate_all_tables`), the table and index counters' start is unknown:
    // named for what it may be, with no span.
    if counters_may_be_reset(snapshot) {
        return Counted::Since(t!("support.since.performance_schema"));
    }
    let since = if counters_since_reset(snapshot) { t!("support.since.reset") } else { t!("support.since.restart") };
    match counter_days(snapshot) {
        Some(days) if days >= 2.0 => Counted::Days { days: days_counted(days), since },
        Some(days) if days * 24.0 < 1.0 => Counted::Minutes { n: (days * 24.0 * 60.0).round().max(1.0), since },
        // An hour and a half or more rounds to 2 hours: "in the hour" is under that.
        Some(days) => Counted::Hours { n: (days * 24.0).round(), since },
        None => Counted::Since(since),
    }
}

/// When the server's counters count from, for a sentence about what they counted: "in the 3 days since the server
/// last restarted", "since userstat was turned on or its statistics last flushed".
pub fn since_phrase(snapshot: &Snapshot) -> String {
    match counted_since(snapshot) {
        Counted::Since(since) => since,
        Counted::Days { days, since } => t!("support.since.days", days = days, since = since),
        Counted::Hours { n, since } => t!("support.since.hours", n = n, since = since),
        Counted::Minutes { n, since } => t!("support.since.minutes", n = n, since = since),
    }
}

/// Days counted, never more than were: a tenth under 10 (9.51 days are "9.5", not "10"), whole days from 10 on. One
/// implementation for prose, metrics (`format::days`) and the app's `format.ts`.
pub use culprit_core::format::days_counted;

/// "once", never "1 times": a count of runs in a sentence.
fn times(n: f64) -> String {
    let n = n.round();
    t!("support.times", n = n, count = format::count(n))
}

/// "about 48K times a day".
fn about_a_day(per_day: f64) -> String {
    t!("support.runs.per_day", times = times(per_day))
}

/// "48K times in the last 6 days".
fn times_in(runs: f64, window: String) -> String {
    t!("support.runs.in", times = times(runs), window = window)
}

/// "about 48K times a day" over a day or more of history, otherwise the real count: extrapolating 35 minutes to a day
/// misleads, and so does "0.2 times a day" for one run in six days.
pub fn runs_phrase(snapshot: &Snapshot, executions: f64) -> String {
    match &snapshot.workload {
        Some(w) if w.measurement_hours() >= 20.0 && w.per_day(executions) < 1.0 => {
            times_in(executions, w.window_phrase())
        }
        Some(w) if w.measurement_hours() >= 20.0 => about_a_day(w.per_day(executions)),
        // A window nobody can date (a log without timestamps, counters with no reset time) isn't "1 minute".
        Some(w) if w.window_unknown() => {
            t!("support.runs.over", times = times(executions), window = w.window_phrase())
        }
        Some(w) => times_in(executions, culprit_core::span_words(w.measurement_hours())),
        None => times(executions),
    }
}

/// How often statement `q` ran, for a sentence that names its window right after it ("It ran {} at 2 ms on average:
/// 25% of all database time in the last 6 days"): the count and, where it's part of the answer, when
/// ([`culprit_core::Workload::cadence`]). A rate a day only for runs that came that way: "750 times" and ", on Oct 11,
/// 15:51–15:55 UTC" for a burst, the count alone for fewer runs than the window has days.
pub fn statement_runs(snapshot: &Snapshot, q: &QueryStat) -> (String, String) {
    let Some(w) = snapshot.workload.as_ref() else { return (times(q.executions), String::new()) };
    let cadence = w.cadence(q);
    let when = match (cadence.when(), &cadence) {
        (Some(when), culprit_core::Cadence::Burst { .. }) => format!(", {when}"),
        (Some(when), _) => format!(" {when}"),
        (None, _) => String::new(),
    };
    let count = match cadence {
        culprit_core::Cadence::Daily(per_day) | culprit_core::Cadence::DailyWithin { per_day, .. } => {
            about_a_day(per_day)
        }
        culprit_core::Cadence::Burst { runs, .. } => times(runs),
        // The window the sentence names holds them.
        culprit_core::Cadence::Counted(runs) if w.measurement_hours() >= culprit_core::PER_DAY_MIN_HOURS => times(runs),
        culprit_core::Cadence::Counted(runs) => runs_phrase(snapshot, runs),
    };
    (count, when)
}

/// How often statement `q` ran, said whole for a title or a sentence that names no window after it: "30K times on Oct
/// 17 at 10:17 UTC" for a burst, "120K times on Oct 11 and Oct 17" for runs on a few days, "about 4.2K times a day"
/// where its runs came that way, otherwise [`runs_phrase`]'s count and window ("4.0K times in 2 hours").
pub fn runs_of(snapshot: &Snapshot, q: &QueryStat) -> String {
    let Some(w) = snapshot.workload.as_ref() else { return times(q.executions) };
    let cadence = w.cadence(q);
    // On a source of running totals, what Culprit's scans measured where they did: "about 31K times a day over 89
    // hours of Culprit's scans".
    if let Some(s) = w.scans_of(q).filter(|_| !matches!(cadence, culprit_core::Cadence::Burst { .. })) {
        let per_day = s.per_day(s.runs);
        return if per_day >= 1.0 {
            t!("support.runs.per_day_over", times = times(per_day), span = s.phrase())
        } else {
            t!("support.runs.over", times = times(s.runs), window = s.phrase())
        };
    }
    match (&cadence, cadence.when()) {
        (culprit_core::Cadence::Burst { runs, .. }, Some(when)) => {
            t!("support.runs.when", times = times(*runs), when = when)
        }
        (culprit_core::Cadence::Daily(per_day) | culprit_core::Cadence::DailyWithin { per_day, .. }, when) => {
            match when {
                Some(when) => t!("support.runs.per_day_when", times = times(*per_day), when = when),
                None => about_a_day(*per_day),
            }
        }
        _ => runs_phrase(snapshot, q.executions),
    }
}

/// How often statements ran together, said whole: [`runs_of`] for one; for several a rate a day only where every one
/// of them ran that way ([`culprit_core::Workload::runs_per_day_of`]), otherwise what was counted and over how long.
pub fn runs_of_all(snapshot: &Snapshot, queries: &[&QueryStat]) -> String {
    let runs: f64 = queries.iter().map(|q| q.executions).sum();
    match (queries, snapshot.workload.as_ref()) {
        ([q], _) => runs_of(snapshot, q),
        (_, Some(w))
            if w.measurement_hours() >= culprit_core::PER_DAY_MIN_HOURS
                && queries.iter().any(|q| w.runs_per_day_of(q).is_none()) =>
        {
            times_in(runs, w.window_phrase())
        }
        _ => runs_phrase(snapshot, runs),
    }
}

/// Whether statement `q`'s runs came in bursts ([`culprit_core::Cadence::Burst`]): said by when they came, graded on
/// their total.
pub fn ran_in_bursts(snapshot: &Snapshot, q: &QueryStat) -> bool {
    snapshot.workload.as_ref().is_some_and(|w| w.cadence(q).is_burst())
}

/// What Culprit's own scans measured of `q` on a source of running totals, as the sentence that says what it costs,
/// where they measured it ([`culprit_core::Workload::scans_of`]): what runs now, with the totals since the source began
/// counting beside it where those differ twofold ([`culprit_core::Workload::totals_beside`]). `of` names what the share
/// is of ("of all database time"). "It ran about 1.7K times a day at 4.0 ms on average over 74 hours of Culprit's own
/// scans: 43% of all database time in them (about 388 runs a day and 2.6 ms on average over the 1277 days since
/// statistics were reset)."
pub fn scans_sentence(snapshot: &Snapshot, q: &QueryStat, of: &str) -> Option<String> {
    let w = snapshot.workload.as_ref()?;
    let s = w.scans_of(q)?;
    // Runs that came in bursts within the scans are said by when they came, never a rate a day: "It ran 750 times at
    // 5.4 ms on average, on Oct 11, 15:51–15:55 UTC: 3% of all database time in 37 hours of Culprit's own scans".
    let mean = format::duration(s.mean_ms().unwrap_or(q.mean_time_ms));
    let (share, span, totals) = (format::percent(s.share()), s.length(), w.totals_beside(q, &s));
    let average = |runs: f64| if runs > 1.0 { "yes" } else { "no" };
    if let Some(when) = Some(w.cadence(q)).filter(|c| c.is_burst()).and_then(|c| c.when()) {
        let (times, average) = (times(q.executions), average(q.executions));
        return Some(match totals {
            Some(totals) => t!(
                "support.scans.burst_totals",
                times = times,
                mean = mean,
                average = average,
                when = when,
                share = share,
                of = of,
                span = span,
                totals = totals
            ),
            None => t!(
                "support.scans.burst",
                times = times,
                mean = mean,
                average = average,
                when = when,
                share = share,
                of = of,
                span = span
            ),
        });
    }
    let per_day = s.per_day(s.runs);
    let runs = if per_day >= 1.0 { about_a_day(per_day) } else { times(s.runs) };
    let average = average(s.runs);
    Some(match totals {
        Some(totals) => t!(
            "support.scans.steady_totals",
            runs = runs,
            mean = mean,
            average = average,
            span = span,
            share = share,
            of = of,
            totals = totals
        ),
        None => {
            t!("support.scans.steady", runs = runs, mean = mean, average = average, span = span, share = share, of = of)
        }
    })
}

/// Absolute cost of the same distinct statements the top-findings share counts.
pub(crate) fn top_findings_cost<'a>(
    snapshot: &'a Snapshot,
    findings: impl IntoIterator<Item = &'a Finding>,
) -> Option<String> {
    top_findings_cost_message(snapshot, findings).map(|message| message.render())
}

pub(crate) fn top_findings_cost_message<'a>(
    snapshot: &'a Snapshot,
    findings: impl IntoIterator<Item = &'a Finding>,
) -> Option<Message> {
    query_cost_message(snapshot, &top_queries(snapshot, findings))
}

/// The distinct statements behind the top three active findings.
fn top_queries<'a>(
    snapshot: &'a Snapshot,
    findings: impl IntoIterator<Item = &'a culprit_core::Finding>,
) -> Vec<&'a QueryStat> {
    let Some(w) = snapshot.workload.as_ref() else { return Vec::new() };
    let ids: std::collections::BTreeSet<&str> = findings
        .into_iter()
        .filter(|f| f.is_actionable())
        .take(3)
        .flat_map(|f| f.queries.iter().map(|q| q.id.as_str()))
        .collect();
    w.queries.iter().filter(|q| ids.contains(q.id.as_str())).collect()
}

/// Share of database time behind the top three active findings: their distinct queries, each counted once, so
/// two findings about the same query don't double it. The one number both the summary and the dashboard state. A
/// statement Culprit's scans measured counts at its share over them ([`culprit_core::Workload::statement_share`]).
pub fn top_findings_share<'a>(
    snapshot: &'a Snapshot,
    findings: impl IntoIterator<Item = &'a culprit_core::Finding>,
) -> Option<f64> {
    let w = snapshot.workload.as_ref()?;
    let queries = top_queries(snapshot, findings);
    if queries.is_empty() {
        return None;
    }
    let share: f64 = queries
        .iter()
        .map(|q| if w.scans_of(q).is_some() { w.statement_share(q) } else { w.share(q.total_time_ms) })
        .sum();
    Some(share.min(1.0))
}

/// What the share behind the top three active findings ([`top_findings_share`]) is over, when Culprit's scans measured
/// every statement behind them: "89 hours of Culprit's scans". `None` otherwise: it is over the workload's window.
pub fn top_findings_scans_message<'a>(
    snapshot: &'a Snapshot,
    findings: impl IntoIterator<Item = &'a culprit_core::Finding>,
) -> Option<Message> {
    let w = snapshot.workload.as_ref()?;
    let spans: Option<Vec<culprit_core::ScanSpan>> =
        top_queries(snapshot, findings).iter().map(|q| w.scans_of(q)).collect();
    spans?.into_iter().max_by(|a, b| a.hours.total_cmp(&b.hours)).map(|s| s.phrase_message())
}

/// "the last 7 days" for windowed history, "the 35 minutes since statistics were reset" for cumulative counters.
pub fn window_phrase(workload: &culprit_core::Workload) -> String {
    workload.window_phrase()
}

/// The window a summary names: the one the scan asked for. Query Store keeps whole intervals (an hour by default), so
/// `--window 6h` reads up to an interval more, and the rounding is said when it shows: "the last 6 hours (Query Store
/// keeps whole intervals, so this covers 7)". Data that covers less than asked for is named as it is.
pub fn summary_window_message(snapshot: &Snapshot) -> Option<Message> {
    let w = snapshot.workload.as_ref()?;
    let asked = snapshot.options.as_ref().map(|o| f64::from(o.window_in_minutes()) / 60.0);
    let Some(asked) = asked.filter(|a| {
        w.is_windowed()
            && snapshot.server.engine == Engine::Mssql
            && w.measurement_hours() > *a
            && w.measurement_hours() <= a + 1.5
    }) else {
        return Some(w.window_message());
    };
    use culprit_core::SpanLength::{Days, Hours, Minutes};
    let (said, covered) = (culprit_core::span_length(asked), culprit_core::span_length(w.measurement_hours()));
    let window = culprit_core::last_span_message(asked);
    if said == covered {
        return Some(window);
    }
    // "7" after "6 hours"; the unit too when it differs (in its unit, or one against several).
    let covered = match (said, covered) {
        (Days(a), Days(n)) | (Hours(a), Hours(n)) | (Minutes(a), Minutes(n)) if (a == 1.0) == (n == 1.0) => {
            MessageValue::Digits(n.to_string())
        }
        _ => culprit_core::span_words_message(w.measurement_hours()).into(),
    };
    Some(culprit_core::message!("support.window.query_store_intervals", window = window, covered = covered))
}

/// `value` from the workload window as a daily rate for comparing with a threshold, over at least an hour of data
/// ([`culprit_core::RATE_MIN_HOURS`]); None under that, so minutes of history never pass for a busy day. Sentences that
/// say "a day" use [`culprit_core::Workload::per_day_measured`] or [`runs_phrase`] instead.
pub fn rate_per_day(snapshot: &Snapshot, value: f64) -> Option<f64> {
    snapshot.workload.as_ref()?.rate_per_day(value)
}

/// [`rate_per_day`] for a part of statements' counts (`value` of `queries`' runs, time, rows or log), at the pace their
/// runs came ([`culprit_core::Workload::rate_per_day_of`]): a burst's total, never extrapolated to a day; runs on a few
/// days over those days. With several statements, each one's part in proportion to its time (its runs where none has
/// any). Every statement-level threshold a day (a "busy" bar, an amount behind a severity) is judged on it.
pub fn rate_per_day_of(snapshot: &Snapshot, queries: &[&QueryStat], value: f64) -> Option<f64> {
    let w = snapshot.workload.as_ref()?;
    let (time, runs): (f64, f64) = queries.iter().fold((0.0, 0.0), |(t, r), q| (t + q.total_time_ms, r + q.executions));
    if queries.is_empty() {
        return w.rate_per_day(value);
    }
    queries
        .iter()
        .map(|q| {
            let part = match (time > 0.0, runs > 0.0) {
                (true, _) => q.total_time_ms / time,
                (false, true) => q.executions / runs,
                _ => 1.0 / queries.len() as f64,
            };
            w.rate_per_day_of(q, value * part)
        })
        .sum()
}

/// Whether statement `q`'s `total` (its runs, rows, log…), counted over the workload window, comes to at least `per_day`
/// a day, judged at the rate it ran while it ran ([`rate_per_day_of`], over at least an hour of data), or, in a window
/// shorter than a day, a total that is already that many (the day that window sits in held at least as many). Nothing
/// is extrapolated: a burst passes a "busy" bar only with as many in its own day, and runs on a few days with as many a
/// day of them. 120K runs in four 2-hour stretches on two days are busy; 4,000 in six seconds aren't "57.8K a day", and
/// 14 runs in a minute aren't "20K a day". False when the window is unknown.
pub fn at_least_per_day_of(snapshot: &Snapshot, q: &QueryStat, total: f64, per_day: f64) -> bool {
    let Some(w) = snapshot.workload.as_ref() else { return false };
    if w.window_unknown() {
        return false;
    }
    rate_per_day_of(snapshot, &[q], total).is_some_and(|r| r >= per_day)
        || (w.measurement_hours() > 0.0 && w.measurement_hours() <= 24.0 && total >= per_day)
}

/// Days of cumulative counters (see [`counter_days`]) when they cover at least an hour
/// ([`culprit_core::RATE_MIN_HOURS`]): enough to compare a rate with a threshold. Sentences that say "a day" also need
/// [`per_day_is_measured`], or say what was counted with [`how_many`].
pub fn counter_days_for_rate(snapshot: &Snapshot) -> Option<f64> {
    counter_days(snapshot).filter(|d| d * 24.0 >= culprit_core::RATE_MIN_HOURS)
}

/// A cumulative counter's `total` (since the server started or statistics were reset) as a daily rate for comparing
/// with a threshold (see [`counter_days_for_rate`]).
pub fn counter_rate_per_day(snapshot: &Snapshot, total: f64) -> Option<f64> {
    counter_days_for_rate(snapshot).map(|d| total / d)
}

/// A counter's own declared reset, without borrowing the server or workload clock.
pub fn counter_days_since(snapshot: &Snapshot, reset: &culprit_core::ResetMarker) -> Option<f64> {
    days_between(reset.at()?, &snapshot.taken_at).filter(|days| *days >= 0.0)
}
pub fn counter_rate_per_day_since(snapshot: &Snapshot, total: f64, reset: &culprit_core::ResetMarker) -> Option<f64> {
    if !total.is_finite() || total < 0.0 {
        return None;
    }
    counter_days_since(snapshot, reset)
        .filter(|days| days * 24.0 >= culprit_core::RATE_MIN_HOURS)
        .map(|days| total / days)
}

pub fn lower_set(items: &[String]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|s| s.to_ascii_lowercase()).collect();
    v.sort();
    v.dedup();
    v
}

pub use culprit_core::SettingChange;

/// How to change a server setting where this server runs. On a server you manage it is SQL, as the engine spells it
/// (`EngineSpec::change_setting`: `ALTER SYSTEM` and a reload on PostgreSQL, `SET PERSIST`/`SET GLOBAL` on MySQL and
/// MariaDB…); a managed service refuses those, so there it is the platform's own control (a DB parameter group, server
/// parameters, database flags) and no SQL. A setting the engine keeps per database, which the database's owner may set
/// (`EngineSpec::database_setting`: PostgreSQL's `random_page_cost`, `work_mem`), is set for this database first there:
/// it changes nothing else on the instance and needs no parameter group or reboot; the platform's control is the
/// route for every database. Every check that suggests a setting words it with this, so advice is always something the
/// reader can run.
pub fn setting_change(server: &culprit_core::ServerInfo, name: &str, value: &str) -> SettingChange {
    // A managed service changes server settings in its own control, not with SQL (`Platform::settings_control`).
    if let Some(control) = server.platform().and_then(|p| p.settings_control()) {
        if let Some(mut database) = server.engine.spec().database_setting(server, name, value) {
            database.how.push(' ');
            database.how.push_str(&t!("support.setting.every_database", control = control));
            return database;
        }
        return SettingChange {
            how: t!("support.setting.in_control", name = name, value = value, control = control),
            sql: None,
        };
    }
    server.engine.spec().change_setting(server, name, value)
}

/// Live shows who holds a lock only while someone waits for it: waits that come less than once an hour over the data's
/// window (`waits` of them, at least) are too few to count on catching one. A window that can't be dated says nothing
/// of how often, and isn't too rare.
pub fn too_rare_to_catch(snapshot: &Snapshot, waits: f64) -> bool {
    snapshot
        .workload
        .as_ref()
        .filter(|w| !w.window_unknown() && w.measurement_hours() > 0.0)
        .is_some_and(|w| waits.ceil() / w.measurement_hours() < 1.0)
}

/// What to find a lock wait's holder with ([`find_lock_holder`]), and the SQL that sets it up where that is SQL.
pub struct LockHolder {
    pub lead: String,
    pub sql: Option<String>,
}

/// How to see the next lock waits too rare to catch live ([`too_rare_to_catch`]), and, with `slower_than` (ms), the
/// next runs slower than that, whatever they waited for: the engine's own log, set the way settings are set on this
/// server, or what the engine already keeps. "Have the next ones logged instead. …", and its SQL where it has one.
pub fn log_next_waits(snapshot: &Snapshot, slower_than: Option<f64>) -> LockHolder {
    let server = &snapshot.server;
    let on = |name: &str| snapshot.setting(name).is_some_and(|s| crate::config::is_on(&s.value));
    match server.engine {
        Engine::Yugabyte => LockHolder { lead: t!("support.lock_holder.yugabyte"), sql: None },
        Engine::Postgres => {
            let deadlock = match snapshot.setting("deadlock_timeout").map(|s| s.value.trim().to_string()) {
                Some(v) if v.parse::<f64>().is_ok() => t!("common.duration.ms", n = v),
                Some(v) => v,
                None => t!("support.lock_holder.postgres.deadlock_default"),
            };
            // A value for the setting, as PostgreSQL reads it (never translated): "2.5s", "800ms".
            let threshold =
                slower_than.map(|ms| if ms >= 1_000.0 { format!("{}s", ms / 1_000.0) } else { format!("{ms}ms") });
            let mut changes: Vec<(&str, &str)> = Vec::new();
            let already = on("log_lock_waits");
            if !already {
                changes.push(("log_lock_waits", "on"));
            }
            if let Some(t) = threshold.as_deref() {
                changes.push(("log_min_duration_statement", t));
            }
            if changes.is_empty() {
                return LockHolder {
                    lead: t!("support.lock_holder.postgres.on_already", deadlock = deadlock),
                    sql: None,
                };
            }
            let on = if already { "yes" } else { "no" };
            let logs = match threshold.as_deref() {
                Some(threshold) => {
                    t!("support.lock_holder.postgres.logs_slow", on = on, deadlock = deadlock, threshold = threshold)
                }
                None => t!("support.lock_holder.postgres.logs", on = on, deadlock = deadlock),
            };
            let change = crate::config::settings_change(server, &changes);
            let lead = [t!("support.lock_holder.logged_instead"), change.how, logs].join(" ");
            LockHolder { lead, sql: change.sql }
        }
        Engine::Mysql => {
            // A run worth logging: the bar asked for, else the second the slow-log finding suggests; never a raise.
            let seconds = slower_than.map_or(1.0, |ms| ms / 1_000.0);
            let current = snapshot.setting("long_query_time").and_then(|s| s.value.trim().parse::<f64>().ok());
            let logging = on("slow_query_log") || on("log_slow_query");
            let mut changes: Vec<(&str, String)> = Vec::new();
            if !logging {
                changes.push(("slow_query_log", "ON".into()));
            }
            if current.is_none_or(|c| c > seconds) {
                changes.push(("long_query_time", format!("{seconds}")));
            }
            let bar = format::digits(format!("{}", current.filter(|c| *c <= seconds).unwrap_or(seconds)));
            let keeps = |logging: &str| {
                let keeps =
                    t!("support.lock_holder.mysql.keeps", logging = logging, bar = &bar, server = server.name());
                // MariaDB writing its slow log to a table counts a statement's whole run as its lock time.
                if server.is_mariadb() {
                    [keeps, t!("check.config.slow_query_log.fix_cost.mariadb_lock_time")].join(" ")
                } else {
                    keeps
                }
            };
            if changes.is_empty() {
                return LockHolder {
                    lead: [t!("support.lock_holder.mysql.look"), keeps("already")].join(" "),
                    sql: None,
                };
            }
            let pairs: Vec<(&str, &str)> = changes.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let change = crate::config::settings_change(server, &pairs);
            LockHolder {
                lead: [t!("support.lock_holder.logged_instead"), change.how, keeps("then")].join(" "),
                sql: change.sql,
            }
        }
        // Query Store keeps when a query waited, interval by interval, not who held the lock. On a server whose options
        // can be set, the blocked process report names the holder.
        Engine::Mssql if server.platform().is_some_and(|p| p.is_azure_sql_database()) => {
            LockHolder { lead: t!("support.lock_holder.azure_sql_database"), sql: None }
        }
        Engine::Mssql => LockHolder { lead: t!("support.lock_holder.mssql"), sql: None },
        Engine::Sqlite => LockHolder { lead: t!("support.lock_holder.sqlite"), sql: None },
    }
}

/// Whether [`find_lock_holder`] has what holds `q`'s locks found live, while it waits: it still runs, more than once,
/// and its waits (`waits`, at least; every run without it) come at least once an hour.
pub fn catches_live(snapshot: &Snapshot, q: &QueryStat, waits: Option<f64>) -> bool {
    !crate::ran_once(q)
        && crate::query_stopped(snapshot, q).is_none()
        && !too_rare_to_catch(snapshot, waits.unwrap_or(q.executions))
}

/// How to find what holds the locks statement `q` waits for, in the words every check that names a lock wait uses: Live
/// while it waits, where its waits come at least once an hour (`waits`, at least; every run without it); otherwise the
/// next ones logged ([`log_next_waits`]); and for a statement that no longer runs, nothing to catch until it comes back.
pub fn find_lock_holder(snapshot: &Snapshot, q: &QueryStat, waits: Option<f64>) -> LockHolder {
    let rare = too_rare_to_catch(snapshot, waits.unwrap_or(q.executions));
    // What logs the next ones, by name: no settings are given for a statement that ran once or no longer runs.
    let log = || match snapshot.server.engine {
        Engine::Postgres => t!("support.lock_holder.log.postgres"),
        Engine::Yugabyte => t!("support.lock_holder.log.yugabyte"),
        Engine::Mysql => t!("support.lock_holder.log.mysql"),
        Engine::Mssql if snapshot.server.platform().is_some_and(|p| p.is_azure_sql_database()) => {
            t!("support.lock_holder.log.azure_sql_database")
        }
        Engine::Mssql => t!("support.lock_holder.log.mssql"),
        Engine::Sqlite => t!("support.lock_holder.log.sqlite"),
    };
    // A statement that ran once is history (`ran_once`): nothing is set up for it unless it runs again.
    if crate::ran_once(q) {
        let window = snapshot
            .workload
            .as_ref()
            .map(|w| w.window_phrase())
            .unwrap_or_else(|| t!("support.lock_holder.the_window"));
        return LockHolder { lead: t!("support.lock_holder.ran_once", window = window, log = log()), sql: None };
    }
    if crate::query_stopped(snapshot, q).is_some() {
        return LockHolder {
            lead: if rare {
                t!("support.lock_holder.stopped_rare", log = log())
            } else {
                t!("support.lock_holder.stopped")
            },
            sql: None,
        };
    }
    if catches_live(snapshot, q, waits) {
        return LockHolder { lead: t!("support.lock_holder.live"), sql: None };
    }
    let logged = log_next_waits(snapshot, None);
    let window = snapshot.workload.as_ref().map(|w| w.window_phrase()).unwrap_or_default();
    let few = match waits {
        Some(n) => t!("support.lock_holder.few_waits", n = n.ceil(), count = format::count(n.ceil()), window = window),
        None => t!("support.lock_holder.few_runs", runs = runs_of(snapshot, q)),
    };
    LockHolder { lead: [few, logged.lead].join(" "), sql: logged.sql }
}

/// A statistic a plan was compiled with, as SQL Server's plan XML names it (its `StatisticsInfo`, kept in the plan's raw
/// XML; the parser keeps nothing else of it): unquoted names.
pub struct PlanStatistic {
    pub schema: String,
    pub table: String,
    pub name: String,
}

/// The statistics `plan` was compiled with ([`PlanStatistic`]): none for a plan without raw XML, or another engine's.
pub fn plan_statistics(plan: &culprit_core::Plan) -> Vec<PlanStatistic> {
    let Some(raw) = plan.raw.as_deref() else { return Vec::new() };
    let attr = |tag: &str, key: &str| -> Option<String> {
        let at = tag.find(&format!(" {key}=\""))? + key.len() + 3;
        Some(tag[at..].split('"').next()?.trim_matches(['[', ']']).to_string())
    };
    raw.split("<StatisticsInfo ")
        .skip(1)
        .filter_map(|rest| {
            let tag = format!(" {}", rest.split('>').next()?);
            Some(PlanStatistic {
                schema: attr(&tag, "Schema")?,
                table: attr(&tag, "Table")?,
                name: attr(&tag, "Statistics")?,
            })
        })
        .collect()
}
