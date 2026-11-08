//! PostgreSQL configuration and `pg_stat_statements` eviction checks.

use super::*;

/// What PostgreSQL's settings establish, one finding each.
pub enum Case<'s> {
    TrackIoTiming,
    AutovacuumOff,
    /// Big tables that churn and follow the server's scale factor (`factor`), the biggest first in `biggest`.
    AutovacuumScale {
        factor: f64,
        big: Vec<&'s culprit_core::TableStat>,
        biggest: &'s culprit_core::TableStat,
    },
    StatementsNotTracked,
    /// pg_stat_statements ran out of room: its evictions, and the rounds since the scan the count is read against.
    StatementsEvicted {
        evicted: Evictions,
        since: Option<(&'s str, f64, f64)>,
        severity: Severity,
    },
    /// shared_buffers this small beside this much data; `slow_io`: this database's measured I/O time is real.
    SharedBuffers {
        shared: f64,
        data: f64,
        slow_io: bool,
    },
    /// WAL buffers filled this many times.
    WalBuffers {
        full: f64,
    },
}

pub(super) fn decide(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    // Planner cost constants are a model of this installation's cache and storage. A default ratio, including on
    // managed storage, proves no slow plan and justifies no database-wide change. Keep them visible in settings.
    if value(snapshot, &["track_io_timing"]).is_some_and(|v| !is_on(v)) {
        out.push(Case::TrackIoTiming);
    }
    out.extend(health(snapshot));
    out
}

fn health(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out = Vec::new();
    if value(snapshot, &["autovacuum"]).is_some_and(|v| !is_on(v)) {
        out.push(Case::AutovacuumOff);
    }

    // Big tables that churn and still follow the server's scale factor. A table that only grows (no updates or
    // deletes) leaves no dead rows for the factor to wait for, and one with its own factor isn't following it.
    let factor = number(snapshot, "autovacuum_vacuum_scale_factor");
    let big: Vec<&culprit_core::TableStat> = snapshot
        .tables
        .iter()
        .filter(|t| t.rows.unwrap_or(0.0) >= 10_000_000.0)
        .filter(|t| {
            t.storage
                .as_ref()
                .is_none_or(|s| s.option("autovacuum_vacuum_scale_factor").is_none() && !s.autovacuum_off())
        })
        .filter(|t| {
            let churn = t.updates.unwrap_or(0.0) + t.deletes.unwrap_or(0.0);
            t.updates.is_none() && t.deletes.is_none() || churn >= t.rows.unwrap_or(0.0) * 0.01
        })
        .collect();
    if let Some(factor) = factor.filter(|v| *v >= 0.2)
        && !big.is_empty()
    {
        let biggest = big
            .iter()
            .copied()
            .max_by(|a, b| a.rows.unwrap_or(0.0).total_cmp(&b.rows.unwrap_or(0.0)))
            .expect("non-empty");
        out.push(Case::AutovacuumScale { factor, biggest, big });
    }

    if value(snapshot, &["pg_stat_statements.track"]).is_some_and(|v| v.eq_ignore_ascii_case("none")) {
        out.push(Case::StatementsNotTracked);
    }

    if let Some(rounds) = number(snapshot, "pg_stat_statements.dealloc").filter(|d| *d > 0.0) {
        // The rounds since the scan the count is read against (the oldest of Culprit's scans a day or more back, else
        // the previous one), an hour or more ago, when it's the same count (no reset since): what goes on now, where
        // the lifetime count can't clear after the fix.
        let since = snapshot
            .evictions_before
            .as_ref()
            .filter(|b| !b.restarted)
            .and_then(|b| {
                Some((b.taken_at.as_str(), (rounds - b.count).max(0.0), days_between(&b.taken_at, &snapshot.taken_at)?))
            })
            .filter(|(_, _, days)| *days >= 1.0 / 24.0);
        // Long enough without a round: the table has had room since, whatever the count says about before. After the
        // limit was raised, that is as long as the room it added takes to fill (`Evictions::quiet_days`).
        let evicted = Evictions::of(snapshot, rounds, since);
        let quiet = evicted.quiet_days(evicted.raised_room());
        if !since.is_some_and(|(_, new, days)| new == 0.0 && days >= quiet) {
            let severity = evictions_severity(snapshot, &evicted, since);
            out.push(Case::StatementsEvicted { evicted, since, severity });
        }
    }

    let shared = value(snapshot, &["shared_buffers"]).and_then(bytes);
    let data = data_bytes(snapshot);
    if !snapshot.server.is_managed()
        && let Some(shared) = shared
        && shared <= 128.0 * 1024.0 * 1024.0
        && data >= 2.0 * 1024.0 * 1024.0 * 1024.0
    {
        // A miss can be an operating-system cache hit. Only this database's measured I/O time establishes a cost.
        let io_ms: f64 = snapshot.waits.iter().filter(|w| category_key(&w.category) == "io").map(|w| w.wait_ms).sum();
        let runtime = snapshot.workload.as_ref().map(|w| w.total_time_ms).unwrap_or(0.0);
        let slow_io = runtime > 0.0 && severity_for_time(snapshot, io_ms, io_ms / runtime) >= Severity::Medium;
        out.push(Case::SharedBuffers { shared, data, slow_io });
    }

    if let Some(full) = snapshot.pressure.as_ref().and_then(|p| p.signal("wal-buffers-full")).map(|s| s.value)
        && counter_rate_per_day(snapshot, full).is_some_and(|r| r >= 24_000.0)
        && value(snapshot, &["wal_buffers"]).and_then(bytes).is_none_or(|b| b < 64.0 * 1024.0 * 1024.0)
    {
        out.push(Case::WalBuffers { full });
    }
    out
}

pub(super) fn render(snapshot: &Snapshot, case: Case<'_>) -> Finding {
    match case {
        Case::TrackIoTiming => {
            let change = setting_change(&snapshot.server, "track_io_timing", "on");
            let mut f = setting_finding(
                "track_io_timing",
                Severity::Low,
                t!("check.config.track_io_timing.title"),
                t!("check.config.track_io_timing.summary"),
                Fix {
                    summary: [t!("check.config.track_io_timing.fix"), change.how].join(" "),
                    sql: change.sql,
                    cost: None,
                },
            );
            f.impact.score = 4.0;
            f
        }
        Case::AutovacuumOff => {
            let change = setting_change(&snapshot.server, "autovacuum", "on");
            let mut f = setting_finding(
                "autovacuum",
                Severity::High,
                t!("check.config.autovacuum.title"),
                t!("check.config.autovacuum.summary"),
                Fix { summary: [t!("check.config.autovacuum.fix"), change.how].join(" "), sql: change.sql, cost: None },
            );
            f.confidence = Confidence::High;
            f
        }
        Case::AutovacuumScale { factor, big, biggest } => {
            let rows = biggest.rows.unwrap_or(0.0);
            let mut f = setting_finding(
                "autovacuum-scale",
                Severity::Low,
                t!(
                    "check.config.autovacuum_scale.title",
                    share = format::percent(factor),
                    table = &biggest.name
                ),
                t!(
                    "check.config.autovacuum_scale.summary",
                    factor = value(snapshot, &["autovacuum_vacuum_scale_factor"]).unwrap_or("0.2"),
                    table = biggest.qualified(),
                    rows = format::count(rows),
                    changed = format::count(rows * factor)
                ),
                Fix {
                    summary: t!("check.config.autovacuum_scale.fix"),
                    sql: Some(
                        big.iter()
                            .take(5)
                            .map(|t| {
                                format!(
                                    "ALTER TABLE {} SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);",
                                    crate::support::qualified(Engine::Postgres, &t.schema, &t.name)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    cost: None,
                },
            );
            f.confidence = Confidence::Medium;
            // The SQL names the five biggest: more tables may follow the server's factor after it.
            f.verify = vec![if big.len() > 5 {
                t!("check.config.autovacuum_scale.verify.more_tables")
            } else {
                t!("check.config.autovacuum_scale.verify.gone")
            }];
            f
        }
        Case::StatementsNotTracked => {
            let change = setting_change(&snapshot.server, "pg_stat_statements.track", "top");
            setting_finding(
                "pg_stat_statements.track",
                Severity::Medium,
                t!("check.config.statements_track.title"),
                t!("check.config.statements_track.summary"),
                Fix {
                    summary: [t!("check.config.statements_track.fix"), change.how].join(" "),
                    sql: change.sql,
                    cost: None,
                },
            )
        }
        Case::StatementsEvicted { evicted, since, severity } => statements_evicted(snapshot, &evicted, since, severity),
        Case::SharedBuffers { shared, data, slow_io } => {
            let title = if (shared - 128.0 * 1024.0 * 1024.0).abs() < 1.0 {
                t!("check.config.shared_buffers.title_default")
            } else {
                t!("check.config.shared_buffers.title", size = format::bytes(shared))
            };
            let mut f = setting_finding(
                "shared_buffers",
                if slow_io { Severity::Medium } else { Severity::Low },
                title,
                t!("check.config.shared_buffers.summary", size = format::bytes(shared), data = format::bytes(data)),
                Fix {
                    summary: t!("check.config.shared_buffers.fix"),
                    // The server's memory isn't readable over SQL: the size is the reader's to fill in, never a guess
                    // that could keep a small server from starting.
                    sql: slow_io.then(||
                        "-- A quarter of the server's memory (4GB on a 16 GB server); takes effect after a restart.\nALTER SYSTEM SET shared_buffers = '<a quarter of RAM>';"
                            .into(),
                    ),
                    cost: Some(t!("check.config.shared_buffers.fix_cost")),
                },
            );
            f.confidence = Confidence::Medium;
            after_restart(&mut f);
            f
        }
        Case::WalBuffers { full } => {
            let current = match value(snapshot, &["wal_buffers"]) {
                Some(current) => current.to_string(),
                None => t!("check.config.wal_buffers.auto_sized"),
            };
            let change = setting_change(&snapshot.server, "wal_buffers", "64MB");
            let (count, since) = (format::count(full), since_phrase(snapshot));
            let mut f = setting_finding(
                "wal_buffers",
                Severity::Low,
                t!("check.config.wal_buffers.title", n = full, count = &count, since = &since),
                t!("check.config.wal_buffers.summary", current = current, n = full, count = &count, since = &since),
                Fix {
                    summary: [t!("check.config.wal_buffers.fix"), change.how, t!("check.config.restart_needed")]
                        .join(" "),
                    sql: change.sql,
                    cost: Some(t!("check.config.wal_buffers.fix_cost")),
                },
            );
            f.confidence = Confidence::Medium;
            f.metrics.push(Metric::said(
                culprit_core::key!("check.config.wal_buffers.metric.full_writes"),
                full,
                Unit::Count,
            ));
            after_restart(&mut f);
            f
        }
    }
}

/// "128MB", "4GB", "8192kB" (Postgres' pretty values) to bytes.
fn bytes(text: &str) -> Option<f64> {
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(t.len());
    let (n, unit) = t.split_at(split);
    let n: f64 = n.parse().ok()?;
    let scale = match unit.trim() {
        "B" | "" => 1.0,
        "kB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(n * scale)
}

fn data_bytes(snapshot: &Snapshot) -> f64 {
    snapshot.tables.iter().filter_map(|t| t.total_bytes).sum()
}

/// pg_stat_statements' evictions, counted. `dealloc` counts rounds, not entries: each time the table is full it drops
/// the least-used 5% of its entries, at least 10 (`entry_dealloc`), so the entries lost are about that many times the
/// rounds. A limit above its default may have been raised after some of the rounds: then the default's 5% is the floor.
#[derive(Clone, Copy)]
pub struct Evictions {
    rounds: f64,
    max: Option<f64>,
    default: Option<f64>,
    raised_from: Option<f64>,
    per_round: f64,
    entries: f64,
    /// The entries evicted a day now: over Culprit's scans where they cover a day and saw rounds (`since`), like a
    /// statement's runs on running totals, otherwise over the window the count covers (an hour of it at least). While
    /// the table is full, new statements arrive as fast as old ones are evicted: it is also how fast new room fills.
    per_day: Option<f64>,
    /// The entries evicted a day over the window the count covers, said beside the scans' rate where the two differ
    /// twofold. `None` where `per_day` is it.
    since_reset: Option<f64>,
}

impl Evictions {
    /// `since`: the scan the count is read against (when it was taken, the rounds since, the days between).
    fn of(snapshot: &Snapshot, rounds: f64, since: Option<(&str, f64, f64)>) -> Evictions {
        let max = number(snapshot, "pg_stat_statements.max").filter(|m| *m > 0.0);
        let default = snapshot
            .setting("pg_stat_statements.max")
            .and_then(|s| s.default_value.as_deref())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|d| *d > 0.0);
        let raised_from = default.zip(max).filter(|(d, m)| d < m).map(|(d, _)| d);
        // From a full table, so 5% of the limit; without the limit only the floor of 10 a round is known.
        let per_round = raised_from.or(max).map_or(10.0, per_round_at);
        let entries = rounds * per_round;
        // Evictions since its statistics were reset, as a rate over that same window: a burst long ago isn't today's.
        let all = crate::support::rate_per_day(snapshot, entries);
        // What Culprit's scans measured over a day or more of them, where they saw rounds: what goes on now.
        let scans = since
            .filter(|(_, new, days)| *new > 0.0 && *days >= SCANS_DAY)
            .map(|(_, new, days)| new * per_round / days);
        let (per_day, since_reset) = match scans {
            Some(now) => (Some(now), all),
            None => (all, None),
        };
        Evictions { rounds, max, default, raised_from, per_round, entries, per_day, since_reset }
    }

    /// The rate over the window the count covers, said beside the scans' (`per_day`) where the two differ twofold
    /// ("about 2.9K a day since its statistics were reset").
    fn beside(&self) -> Option<f64> {
        match (self.per_day, self.since_reset) {
            (Some(now), Some(all)) if now >= all * 2.0 || now * 2.0 <= all => Some(all),
            _ => None,
        }
    }

    /// The room a raise of the limit added, at most: from its default to the limit now. None when it wasn't raised.
    fn raised_room(&self) -> f64 {
        self.raised_from.zip(self.max).map_or(0.0, |(d, m)| m - d)
    }

    /// How long a later scan must see no new round before the table counts as having room: a day, or, after the limit
    /// was raised by `room` entries, as long as that room takes to fill at the rate entries are evicted now. Nothing is
    /// evicted while new room fills, whatever the statements do, so a shorter quiet span proves nothing. At most a
    /// week, the span an absence is claimed over everywhere (a weekly job has had its chance): room that takes longer
    /// fills at fewer entries a day than that, and a round after it reopens the finding.
    fn quiet_days(&self, room: f64) -> f64 {
        let fill = match self.per_day {
            _ if room <= 0.0 => 0.0,
            Some(per_day) if per_day > 0.0 => room / per_day,
            _ => 7.0,
        };
        fill.clamp(20.0 / 24.0, 7.0)
    }
}

/// Days of Culprit's scans from which an eviction count says "a day" ([`culprit_core::PER_DAY_MIN_HOURS`]).
const SCANS_DAY: f64 = culprit_core::PER_DAY_MIN_HOURS / 24.0;

/// The entries one round evicts from a full table of `limit`: the least-used 5%, at least 10.
fn per_round_at(limit: f64) -> f64 {
    (limit * 0.05).floor().max(10.0).min(limit)
}

/// How serious the evictions are: Medium where this database's statements fill the table (it holds more than a quarter
/// of the limit) and it loses a thousand entries a day or more, now (over Culprit's scans) or over the count's window
/// where the limit wasn't raised since.
fn evictions_severity(snapshot: &Snapshot, evicted: &Evictions, since: Option<(&str, f64, f64)>) -> Severity {
    let mine = snapshot.workload.as_ref().map(|w| w.query_count).filter(|n| *n > 0.0);
    let fills = mine.zip(evicted.max).is_some_and(|(n, limit)| n * 4.0 > limit);
    let losing = match since.filter(|(_, new, _)| *new > 0.0) {
        Some((_, new, days)) => new * evicted.per_round / days >= 1000.0,
        None => evicted.raised_from.is_none() && evicted.per_day.is_some_and(|d| d >= 1000.0),
    };
    if fills && losing { Severity::Medium } else { Severity::Low }
}

/// The finding about pg_stat_statements' evictions.
fn statements_evicted(
    snapshot: &Snapshot,
    evicted: &Evictions,
    since: Option<(&str, f64, f64)>,
    severity: Severity,
) -> Finding {
    let Evictions { rounds, max, default, raised_from, per_round, entries, per_day: entries_per_day, since_reset } =
        *evicted;
    let over_scans = since_reset.is_some();
    let room = max.map_or(20_000.0, |m| (m * 2.0).max(20_000.0));
    let change = setting_change(&snapshot.server, "pg_stat_statements.max", &format!("{room:.0}"));
    // An estimate at today's limit where it was never raised; a floor otherwise.
    let estimate = if max.is_some() && raised_from.is_none() { "yes" } else { "no" };
    let workload = snapshot.workload.as_ref();
    let reset_on =
        workload.and_then(|w| w.window_start.as_deref()).map(|start| format::friendly_date(start, &snapshot.taken_at));
    let reset = if reset_on.is_some() { "dated" } else { "undated" };
    let reset_on = reset_on.unwrap_or_default();
    let mine = workload.map(|w| w.query_count).filter(|n| *n > 0.0);
    let others = mine.zip(max).filter(|(n, limit)| *n * 4.0 <= *limit).map(|(n, _)| n);
    let fills = mine.zip(max).is_some_and(|(n, limit)| n * 4.0 > limit);
    let max_said = max.map(|m| format!("{m:.0}")).unwrap_or_default();
    let per_day = workload.and_then(|w| w.per_day_measured(entries)).filter(|_| !over_scans);
    // The rounds since Culprit's scan: (rounds, entries, when), and the sentence that says them.
    let recent = since.filter(|(_, new, _)| *new > 0.0).map(|(taken_at, new, days)| {
        let new_entries = new * per_round;
        let when = crate::queries::when_phrase(taken_at);
        let beside = evicted.beside();
        let said = t!(
            "check.config.statements_evicted.summary.recent",
            n = new,
            rounds = format::count(new),
            when = &when,
            estimate = estimate,
            entries = format::count(new_entries),
            rate = if days >= SCANS_DAY { "per_day" } else { "span" },
            per_day = format::count(new_entries / days),
            beside = if beside.is_some() { "yes" } else { "no" },
            since_reset = beside.map(format::count).unwrap_or_default(),
            span = culprit_core::span_words(days * 24.0)
        );
        (new, new_entries, said, when)
    });
    let whose = if others.is_some() { "yes" } else { "no" };
    let fix = match raised_from {
        Some(d) if recent.is_some() => {
            t!("check.config.statements_evicted.fix.raised_growing", default = format!("{d:.0}"), whose = whose)
        }
        Some(d) => t!("check.config.statements_evicted.fix.raised", default = format!("{d:.0}"), whose = whose),
        None => t!("check.config.statements_evicted.fix.keep_more", whose = whose),
    };
    let title = match &recent {
        Some((new, new_entries, _, when)) => t!(
            "check.config.statements_evicted.title.since_scan",
            n = *new,
            rounds = format::count(*new),
            when = when,
            estimate = estimate,
            entries = format::count(*new_entries)
        ),
        None => t!(
            "check.config.statements_evicted.title",
            n = rounds,
            rounds = format::count(rounds),
            estimate = estimate,
            entries = format::count(entries)
        ),
    };
    let mut summary = vec![
        t!(
            "check.config.statements_evicted.summary.limit",
            limit = if max.is_some() { "known" } else { "unknown" },
            max = &max_said
        ),
        t!(
            "check.config.statements_evicted.summary.rounds",
            reset = reset,
            date = &reset_on,
            n = rounds,
            rounds = format::count(rounds),
            each = match (max, raised_from) {
                (Some(_), Some(_)) => "raised",
                (Some(_), None) => "limit",
                (None, _) => "floor",
            },
            per_round_now = max.map(|m| format::count(per_round_at(m))).unwrap_or_default(),
            per_round = format::count(per_round),
            per_round_default = raised_from.map(|d| format::count(per_round_at(d))).unwrap_or_default(),
            max = &max_said,
            default = raised_from.map(|d| format!("{d:.0}")).unwrap_or_default(),
            estimate = estimate,
            entries = format::count(entries),
            rated = if per_day.is_some() { "yes" } else { "no" },
            per_day = per_day.map(format::count).unwrap_or_default()
        ),
    ];
    if let Some((_, _, said, _)) = &recent {
        summary.push(said.clone());
    }
    summary.push(t!("check.config.statements_evicted.summary.incomplete"));
    match others {
        Some(n) => {
            summary.push(t!("check.config.statements_evicted.summary.other_databases", count = format::count(n)))
        }
        None if fills => summary.push(t!("check.config.statements_evicted.summary.distinct_statements")),
        None => {}
    }
    let mut f = setting_finding(
        "pg_stat_statements.max",
        severity,
        title,
        summary.join(" "),
        Fix {
            summary: [fix, change.how, t!("check.config.restart_needed")].join(" "),
            sql: change.sql,
            cost: Some(t!(
                "check.config.statements_evicted.fix_cost",
                size = format::bytes(room * 512.0),
                room = format!("{room:.0}")
            )),
        },
    );
    f.confidence = Confidence::High;
    f.metrics.push(Metric::said(
        culprit_core::key!("check.config.statements_evicted.metric.rounds"),
        rounds,
        Unit::Count,
    ));
    if let Some((new, _, _, when)) = &recent {
        f.metrics.push(Metric::new(
            t!("check.config.statements_evicted.metric.rounds_since_scan", when = when),
            *new,
            Unit::Count,
        ));
    }
    f.metrics.push(Metric::new(
        if estimate == "yes" {
            t!("check.config.statements_evicted.metric.entries_estimate")
        } else {
            t!("check.config.statements_evicted.metric.entries_at_least")
        },
        entries,
        Unit::Count,
    ));
    let added = room - default.filter(|d| *d < room).or(max).unwrap_or(0.0);
    let quiet = evicted.quiet_days(added);
    let span = match quiet.ceil() {
        d if d <= 1.0 => "day",
        d if d >= 7.0 => "week",
        _ => "days",
    };
    let mut verify = vec![t!(
        "check.config.statements_evicted.verify.rescan",
        span = span,
        days = format!("{:.0}", quiet.ceil()),
        rounds = format!("{rounds:.0}")
    )];
    match entries_per_day {
        _ if quiet <= 20.0 / 24.0 => {}
        Some(a_day) => {
            verify.push(t!(
                "check.config.statements_evicted.verify.room_fills",
                added = format::count(added),
                per_day = format::count(a_day),
                measured = if over_scans { "scans" } else { "count" }
            ));
            if added / a_day > 7.0 {
                verify.push(t!("check.config.statements_evicted.verify.after_the_week"));
            }
        }
        None => verify.push(t!("check.config.statements_evicted.verify.rate_unknown")),
    }
    verify.push(t!("check.config.statements_evicted.verify.count_starts_over"));
    f.verify = vec![verify.join(" ")];
    let mut covers = vec![t!("check.config.statements_evicted.caveat.count_covers", reset = reset, date = &reset_on)];
    if default.is_none() {
        covers.push(t!("check.config.statements_evicted.caveat.raised_after"));
    }
    f.caveats.push(covers.join(" "));
    match (max, raised_from) {
        (Some(m), Some(_)) => f.caveats.push(t!(
            "check.config.statements_evicted.caveat.floor",
            per_round = format::count(per_round),
            max = format!("{m:.0}"),
            per_round_now = format::count(per_round_at(m))
        )),
        (Some(_), None) if default.is_none() => f.caveats.push(t!("check.config.statements_evicted.caveat.estimated")),
        _ => {}
    }
    f
}

/// The findings this file makes of `snapshot`.
#[cfg(test)]
pub(super) fn check(snapshot: &Snapshot) -> Vec<Finding> {
    decide(snapshot).into_iter().map(|case| render(snapshot, case)).collect()
}
