//! The slow query log table (`mysql.slow_log`, written when `log_output` includes TABLE) as a workload source: every
//! logged run with its time, lock time and rows, grouped by statement fingerprint. It is the statistics source that
//! needs no restart on servers running without performance_schema (MariaDB's default), and it keeps each run's text
//! and time, so plans, percentiles, a timeline and period comparisons all come from it directly. These are logged
//! runs only: current settings cannot establish historical session thresholds or other logging filters.

use std::collections::HashMap;

use culprit_core::changes::{Period, PeriodData, PeriodStats, QueryPeriods};
use culprit_core::{QueryStat, QueryTrend, Result, Spread, Timeline, TimelineSource, WaitStat, Workload, t};
use mysql_async::Value;

use crate::connect::TAG;
use crate::fingerprint::{self, fingerprint};
use crate::rows::query_with;

/// Bounds returned runs, newest first. The native table has no index, so scanning/sorting still needs the read timeout.
const MAX_ROWS: usize = culprit_core::limits::MAX_RESULT_ROWS;
const MAX_BUCKETS: f64 = 120.0;
const BUCKET_MINUTES: [u32; 10] = [15, 30, 60, 120, 180, 240, 360, 480, 720, 1440];

pub const SOURCE: &str = "slow query log";
const LOCK_TIME_FIELD: &str = "mysql.slow_log.lock_time";

/// One logged run.
pub struct Run {
    pub at: f64,
    pub at_text: String,
    pub ms: f64,
    pub lock_ms: Option<f64>,
    pub rows_sent: f64,
    pub rows_examined: f64,
    pub sql: String,
}

/// TIME(6) columns as milliseconds, whatever protocol the server answers with.
fn ms_of(column: &str) -> String {
    format!(
        "(HOUR({column}) * 3600 + MINUTE({column}) * 60 + SECOND({column}) + MICROSECOND({column}) / 1000000) * 1000"
    )
}

/// Runs logged for `database` between `since` and `until` (UTC, `YYYY-MM-DD HH:MM:SS`), newest first.
pub async fn read(
    conn: &mut impl crate::read::QueryRead,
    database: &str,
    since: &str,
    until: Option<&str>,
) -> Result<Vec<Run>> {
    Ok(read_runs(conn, database, since, until).await?.0)
}

/// [`read`], and whether the read stopped at [`MAX_ROWS`] before reaching `since`.
pub async fn read_runs(
    conn: &mut impl crate::read::QueryRead,
    database: &str,
    since: &str,
    until: Option<&str>,
) -> Result<(Vec<Run>, bool)> {
    let sql = format!(
        "SELECT DATE_FORMAT(start_time, '%Y-%m-%dT%H:%i:%sZ') AS at, UNIX_TIMESTAMP(start_time) AS epoch,
                {query} AS ms, {lock} AS lock_ms, rows_sent, rows_examined, CONVERT(sql_text USING utf8mb4) AS sql_text
           FROM mysql.slow_log
          WHERE db = ? AND start_time >= ? {until}
          ORDER BY start_time DESC LIMIT {MAX_ROWS}",
        query = ms_of("query_time"),
        lock = ms_of("lock_time"),
        until = if until.is_some() { "AND start_time < ?" } else { "" },
    );
    let mut params = vec![Value::from(database), Value::from(since)];
    if let Some(until) = until {
        params.push(Value::from(until));
    }
    let rows = query_with(conn, &sql, params).await?;
    let truncated = rows.len() >= MAX_ROWS;
    let runs = rows.iter().map(decode_run).collect::<Result<Vec<_>>>()?.into_iter().flatten().collect();
    Ok((runs, truncated))
}

fn decode_run(row: &crate::rows::Rec) -> Result<Option<Run>> {
    let Some(sql) = row.text("sql_text").filter(|sql| !ignored(sql)) else { return Ok(None) };
    Ok(Some(Run {
        at: row.number("epoch").ok_or_else(crate::native::invalid)?,
        at_text: row.string("at"),
        ms: milliseconds(row, "ms")?.ok_or_else(crate::native::invalid)?,
        lock_ms: milliseconds(row, "lock_ms")?,
        rows_sent: row.number("rows_sent").unwrap_or(0.0),
        rows_examined: row.number("rows_examined").unwrap_or(0.0),
        sql,
    }))
}

/// SQL NULL is unavailable; missing, malformed or negative timers cannot become a measured zero.
fn milliseconds(row: &crate::rows::Rec, name: &str) -> Result<Option<f64>> {
    match row.number(name) {
        Some(value) if value.is_finite() && value >= 0.0 => Ok(Some(value)),
        Some(_) => Err(crate::native::invalid()),
        None if row.native_text(name)?.is_none() => Ok(None),
        None => Err(crate::native::invalid()),
    }
}

pub(crate) fn lock_waits(milliseconds: Option<f64>) -> Vec<WaitStat> {
    milliseconds
        .filter(|ms| *ms > 0.0)
        .map(|wait_ms| WaitStat {
            category: "Lock".into(),
            wait_type: Some(LOCK_TIME_FIELD.into()),
            wait_ms,
            tasks: None,
        })
        .into_iter()
        .collect()
}

/// Culprit's own statements, plans, and the driver handshakes nobody can act on.
fn ignored(sql: &str) -> bool {
    let head = sql.trim_start().to_ascii_uppercase();
    sql.contains(TAG)
        || head.starts_with("EXPLAIN ")
        || head.starts_with("ANALYZE FORMAT")
        || head.starts_with("SHOW ")
        || head.starts_with("SELECT @@")
        || head.starts_with("SET ")
}

/// Runs of one statement shape.
struct Group {
    key: String,
    display: String,
    sample: String,
    runs: Vec<f64>,
    lock_ms: Option<f64>,
    rows_sent: f64,
    rows_examined: f64,
    first: (f64, String),
    last: (f64, String),
}

fn group(runs: &[Run], prints: &[fingerprint::Fingerprint]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (run, print) in runs.iter().zip(prints) {
        let at = (run.at, run.at_text.clone());
        let i = *index.entry(print.key.clone()).or_insert_with(|| {
            groups.push(Group {
                key: print.key.clone(),
                display: print.display.clone(),
                // Runs arrive newest first: the first one seen is the latest text.
                sample: run.sql.clone(),
                runs: Vec::new(),
                lock_ms: None,
                rows_sent: 0.0,
                rows_examined: 0.0,
                first: at.clone(),
                last: at.clone(),
            });
            groups.len() - 1
        });
        let g = &mut groups[i];
        g.runs.push(run.ms);
        if let Some(ms) = run.lock_ms {
            g.lock_ms = Some(g.lock_ms.unwrap_or(0.0) + ms);
        }
        g.rows_sent += run.rows_sent;
        g.rows_examined += run.rows_examined;
        if at.0 < g.first.0 {
            g.first = at.clone();
        }
        if at.0 > g.last.0 {
            g.last = at;
        }
    }
    groups
}

/// The value below which `q` of the sorted runs fall.
fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((sorted.len() as f64) * q).ceil() as usize;
    sorted.get(rank.saturating_sub(1).min(sorted.len() - 1)).copied()
}

/// Run times in up to 24 buckets on a log scale, fastest first.
fn histogram(sorted: &[f64]) -> Vec<[f64; 3]> {
    let (Some(&low), Some(&high)) = (sorted.first(), sorted.last()) else { return Vec::new() };
    if sorted.len() < 5 || high <= low {
        return Vec::new();
    }
    let (lo, hi) = (low.max(0.001).ln(), high.ln());
    let n = 24usize;
    let width = (hi - lo) / n as f64;
    let mut buckets: Vec<[f64; 3]> =
        (0..n).map(|i| [(lo + width * i as f64).exp(), (lo + width * (i + 1) as f64).exp(), 0.0]).collect();
    for &ms in sorted {
        let i = (((ms.max(0.001).ln() - lo) / width) as usize).min(n - 1);
        buckets[i][2] += 1.0;
    }
    buckets[0][0] = low;
    buckets[n - 1][1] = high.max(buckets[n - 1][0] + f64::EPSILON);
    buckets.retain(|b| b[2] > 0.0);
    buckets
}

fn captured_distribution(sorted: &[f64]) -> culprit_core::RuntimeDistribution {
    use culprit_core::{
        DistributionPopulation, RuntimeDistribution, RuntimeDistributionOrigin, RuntimeDuration,
        RuntimeHistogramBucket, RuntimeQuantile,
    };
    let mut observed = RuntimeDistribution {
        source: "mysql.slow_log".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: sorted.len().to_string() },
        p50: percentile(sorted, 0.5).map(RuntimeQuantile::sample),
        p95: percentile(sorted, 0.95).map(RuntimeQuantile::sample),
        p99: percentile(sorted, 0.99).map(RuntimeQuantile::sample),
        histogram: histogram(sorted)
            .into_iter()
            .map(|[lower, upper, count]| RuntimeHistogramBucket {
                lower: RuntimeDuration::Milliseconds { value: lower },
                upper: RuntimeDuration::Milliseconds { value: upper },
                samples: (count as u64).to_string(),
            })
            .collect(),
        issue: None,
    };
    if !observed.is_valid() {
        observed.issue = Some(culprit_core::RuntimeDistributionIssue::InvalidNativeValues);
    }
    observed
}

pub struct Collected {
    pub workload: Workload,
    /// Observed native lock-time subtotal for these logged runs; unknown values do not become zero.
    pub lock_ms: Option<f64>,
    /// A real statement for each kept query, to plan it.
    pub samples: Vec<(String, String)>,
    pub threshold_note: culprit_core::i18n::Message,
}

/// The workload from logged runs, keeping the heaviest `top` statement shapes by time. The window is the one asked for
/// (`window_start` to `now`): the log was read over all of it, and a quiet stretch with no slow run is still part of
/// it. Only when the read stopped at [`MAX_ROWS`] (`truncated`) does it start at the oldest run read. Totals are of
/// logged runs only. A current `long_query_time` value does not establish their historical logging threshold.
pub fn workload(
    runs: &[Run],
    window_start: &str,
    now: &str,
    threshold_s: Option<f64>,
    top: usize,
    truncated: bool,
) -> Collected {
    let prints: Vec<fingerprint::Fingerprint> = runs.iter().map(|r| fingerprint(&r.sql)).collect();
    let mut groups = group(runs, &prints);
    groups.sort_by(|a, b| b.runs.iter().sum::<f64>().total_cmp(&a.runs.iter().sum::<f64>()));
    let total_time_ms: f64 = runs.iter().map(|r| r.ms).sum();
    let total_executions = runs.len() as f64;
    let query_count = groups.len() as f64;
    let oldest = runs.iter().map(|r| (r.at, &r.at_text)).min_by(|a, b| a.0.total_cmp(&b.0));
    let window_start_text = match oldest {
        Some((_, at)) if truncated => at.clone(),
        _ => window_start.to_string(),
    };
    let window_hours = match (culprit_core::changes::epoch(&window_start_text), culprit_core::changes::epoch(now)) {
        (Some(a), Some(b)) if b > a => (b - a) / 3600.0,
        _ => 1.0 / 60.0,
    };
    let timeline = timeline(runs, &window_start_text, now);
    let mut samples = Vec::new();
    let queries: Vec<QueryStat> = groups
        .into_iter()
        .take(top)
        .map(|g| {
            let id = fingerprint::id(&g.key);
            samples.push((id.clone(), g.sample.clone()));
            let mut sorted = g.runs.clone();
            sorted.sort_by(f64::total_cmp);
            let executions = sorted.len() as f64;
            let total: f64 = sorted.iter().sum();
            let trend = timeline.as_ref().map(|(t, _)| query_trend(runs, &prints, &g.key, t));
            QueryStat {
                plan_history: None,
                variant_family: None,
                resolved_write_target: None,
                requests: None,
                id: id.clone(),
                query_hash: Some(id),
                text: g.display,
                parameters: None,
                object_name: None,
                executions,
                total_time_ms: total,
                mean_time_ms: if executions > 0.0 { total / executions } else { 0.0 },
                max_time_ms: sorted.last().copied(),
                stddev_time_ms: None,
                cpu_time_ms: None,
                rows: Some(g.rows_sent),
                logical_reads: Some(g.rows_examined),
                physical_reads: None,
                temporary_work: None,
                first_seen: Some(g.first.1),
                last_seen: Some(g.last.1),
                plans: Vec::new(),
                plan: None,
                waits: lock_waits(g.lock_ms),
                waits_coverage: None,
                trend,
                costs: None,
                rows_affected: None,
                plan_id: None,
                spread: Some(Spread {
                    min_ms: sorted.first().copied(),
                    distributions: vec![captured_distribution(&sorted)],
                    lock_ms: g.lock_ms,
                }),
            }
        })
        .collect();
    let threshold_note = match threshold_s {
        Some(s) if s.is_finite() && s > 0.0 => {
            culprit_core::message!("provider.mysql.slow_log.threshold", threshold = seconds_message(s))
        }
        Some(0.0) => culprit_core::message!("provider.mysql.slow_log.every_statement"),
        _ => culprit_core::message!("provider.mysql.slow_log.threshold_unknown"),
    };
    Collected {
        lock_ms: runs.iter().filter_map(|r| r.lock_ms).reduce(|a, b| a + b),
        workload: Workload {
            source_label: None,
            source_message: None,
            counter_set: None,
            history_inputs: Vec::new(),
            source: SOURCE.into(),
            kind: Some(culprit_core::WorkloadKind::SlowLog),
            windowed: true,
            window_start: Some(window_start_text),
            window_end: Some(now.to_string()),
            window_hours,
            total_time_ms,
            total_cpu_ms: None,
            total_executions,
            query_count,
            queries,
            timeline: timeline.map(|(t, _)| t),
            untracked_time_ms: None,
            hourly_time_ms: None,
            min_logged_ms: None,
            log: None,
        },
        samples,
        threshold_note,
    }
}

/// long_query_time as the server's setting reads: whole milliseconds under a second, seconds as set above.
fn seconds_message(s: f64) -> culprit_core::i18n::Message {
    if s < 1.0 {
        culprit_core::message!(
            "common.duration.ms",
            n = culprit_core::i18n::MessageValue::Digits(format!("{:.0}", s * 1000.0))
        )
    } else {
        culprit_core::message!("common.duration.s", n = culprit_core::i18n::MessageValue::Digits(format!("{s}")))
    }
}

/// Equal buckets from the window start to now: a day in 15-minute buckets, a week in 2-hour ones.
fn timeline(runs: &[Run], start: &str, end: &str) -> Option<(Timeline, f64)> {
    let (a, b) = (culprit_core::changes::epoch(start)?, culprit_core::changes::epoch(end)?);
    if runs.is_empty() || b <= a {
        return None;
    }
    let first = runs.iter().map(|r| r.at).fold(f64::INFINITY, f64::min).max(a);
    let minutes =
        BUCKET_MINUTES.into_iter().find(|&m| (b - first) / 60.0 / f64::from(m) <= MAX_BUCKETS).unwrap_or(1440);
    let size = f64::from(minutes) * 60.0;
    let first_bucket = (first / size).floor();
    let len = ((b / size).floor() - first_bucket + 1.0).max(1.0) as usize;
    let mut t = Timeline {
        source: TimelineSource::SlowLog,
        starts: (0..len).map(|i| iso((first_bucket + i as f64) * size)).collect(),
        end: end.to_string(),
        time_ms: vec![0.0; len],
        executions: vec![0.0; len],
        unknown: None,
    };
    for run in runs {
        let i = ((run.at / size).floor() - first_bucket).clamp(0.0, len as f64 - 1.0) as usize;
        t.time_ms[i] += run.ms;
        t.executions[i] += 1.0;
    }
    t.time_ms.iter_mut().for_each(|v| *v = (*v * 100.0).round() / 100.0);
    Some((t, size))
}

fn query_trend(runs: &[Run], prints: &[fingerprint::Fingerprint], key: &str, timeline: &Timeline) -> QueryTrend {
    let len = timeline.len();
    let mut trend = QueryTrend { time_ms: vec![Some(0.0); len], executions: vec![Some(0.0); len] };
    let starts: Vec<f64> = timeline.starts.iter().filter_map(|s| culprit_core::changes::epoch(s)).collect();
    for (run, _) in runs.iter().zip(prints).filter(|(_, p)| p.key == key) {
        let i = starts.iter().rposition(|s| *s <= run.at).unwrap_or(0);
        if let (Some(Some(t)), Some(Some(e))) = (trend.time_ms.get_mut(i), trend.executions.get_mut(i)) {
            *t += run.ms;
            *e += 1.0;
        }
    }
    trend
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since 1970 (UTC).
pub fn iso(epoch: f64) -> String {
    let secs = epoch.floor() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `2026-10-11T07:00:00Z` → `2026-10-11 07:00:00`, the form MySQL compares DATETIME and TIMESTAMP values with.
pub fn sql_time(iso: &str) -> String {
    iso.trim_end_matches('Z').replace('T', " ")
}

/// Each statement shape's runs in two periods, straight from the log.
pub fn periods(runs: &[Run], baseline: &Period, recent: &Period, logged_since: Option<f64>) -> PeriodData {
    let epoch = |s: &str| culprit_core::changes::epoch(s).unwrap_or(0.0);
    let (b0, b1, r0, r1) = (epoch(&baseline.start), epoch(&baseline.end), epoch(&recent.start), epoch(&recent.end));
    let mut data = PeriodData {
        source: TimelineSource::SlowLog,
        baseline: baseline.clone(),
        recent: recent.clone(),
        ..Default::default()
    };
    let mut by_key: HashMap<String, QueryPeriods> = HashMap::new();
    let add = |stats: &mut Option<PeriodStats>, ms: f64| {
        let mut one = PeriodStats::new(1.0, ms);
        one.min_ms = Some(ms);
        one.max_ms = Some(ms);
        match stats {
            Some(s) => s.add(&one),
            None => *stats = Some(one),
        }
    };
    let mut baseline_total: Option<PeriodStats> = None;
    let mut recent_total: Option<PeriodStats> = None;
    for run in runs {
        let in_baseline = run.at >= b0 && run.at < b1;
        let in_recent = run.at >= r0 && run.at < r1;
        if !in_baseline && !in_recent {
            continue;
        }
        let print = fingerprint(&run.sql);
        let q = by_key.entry(print.key.clone()).or_insert_with(|| QueryPeriods {
            query_id: fingerprint::id(&print.key),
            text: print.display.clone(),
            ..Default::default()
        });
        if in_baseline {
            add(&mut q.baseline, run.ms);
            add(&mut baseline_total, run.ms);
        }
        if in_recent {
            add(&mut q.recent, run.ms);
            add(&mut recent_total, run.ms);
        }
    }
    data.baseline_total = baseline_total.unwrap_or_else(|| PeriodStats::new(0.0, 0.0));
    data.recent_total = recent_total.unwrap_or_else(|| PeriodStats::new(0.0, 0.0));
    // The log only covers the time since it was turned on (or last emptied).
    let since = logged_since.unwrap_or(b0);
    data.baseline_coverage = if b1 > b0 { ((b1 - since.max(b0)) / (b1 - b0)).clamp(0.0, 1.0) } else { 0.0 };
    data.queries = by_key.into_values().collect();
    data.notes.push(t!("provider.mysql.slow_log.periods"));
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_run(lock_time: Value) -> crate::rows::Rec {
        crate::rows::Rec::from_values(vec![
            ("at", Value::from("2026-11-13T08:00:00Z")),
            ("epoch", Value::UInt(1_794_556_800)),
            ("ms", Value::from("12.345")),
            ("lock_ms", lock_time),
            ("sql_text", Value::from("SELECT value FROM t WHERE id=1")),
        ])
    }

    #[test]
    fn logged_lock_time_preserves_native_unknown_zero_and_fractional_subtotals() {
        use culprit_core::telemetry::WaitClass;
        let decode = |value| decode_run(&native_run(value)).unwrap().unwrap();
        let mut runs = vec![decode(Value::NULL)];
        let collected = workload(&runs, "2026-11-13T07:00:00Z", "2026-11-13T09:00:00Z", None, 10, false);
        let q = &collected.workload.queries[0];
        assert_eq!(collected.lock_ms, None);
        assert_eq!(q.spread.as_ref().unwrap().lock_ms, None);
        assert!(q.waits.is_empty());
        assert_eq!(q.measured_wait_ms(WaitClass::Lock), None);

        runs.push(decode(Value::from("0.000")));
        let collected = workload(&runs, "2026-11-13T07:00:00Z", "2026-11-13T09:00:00Z", Some(0.0), 10, false);
        let q = &collected.workload.queries[0];
        assert_eq!(q.spread.as_ref().unwrap().lock_ms, Some(0.0));
        assert!(q.waits.is_empty());
        assert_eq!(q.measured_wait_ms(WaitClass::Lock), None, "native zero is not complete canonical Lock coverage");

        runs.push(decode(Value::from("19.876")));
        let collected = workload(&runs, "2026-11-13T07:00:00Z", "2026-11-13T09:00:00Z", Some(10.0), 10, false);
        let q = &collected.workload.queries[0];
        assert_eq!(q.spread.as_ref().unwrap().lock_ms, Some(19.876));
        assert_eq!(q.waits[0].wait_ms, 19.876);
        assert_eq!(q.waits[0].wait_type.as_deref(), Some("mysql.slow_log.lock_time"));
        assert_eq!(q.waits_coverage, None);
        assert_eq!(q.measured_wait_ms(WaitClass::Lock), None);
        assert_eq!(
            collected.workload.min_logged_ms, None,
            "a present-day setting cannot manufacture a historical floor"
        );
    }

    #[test]
    fn malformed_native_log_timers_cannot_become_zero() {
        for malformed in [Value::Int(-1), Value::Double(f64::INFINITY), Value::Double(f64::NAN), Value::from("broken")]
        {
            assert!(decode_run(&native_run(malformed)).is_err());
        }
        assert!(milliseconds(&crate::rows::Rec::from_values(vec![]), "lock_ms").is_err());
        let row = crate::rows::Rec::from_values(vec![("ms", Value::NULL)]);
        assert_eq!(milliseconds(&row, "ms").unwrap(), None);
    }

    struct LoggedRead {
        rows: Vec<crate::rows::Rec>,
        parameters: Vec<Value>,
        sql: String,
    }

    #[async_trait::async_trait]
    impl crate::read::QueryRead for LoggedRead {
        async fn read(&mut self, sql: &str, parameters: Vec<Value>, row_limit: usize) -> Result<Vec<crate::rows::Rec>> {
            assert!(sql.ends_with(&format!("LIMIT {row_limit}")), "the SQL limit must fit its owned reader budget");
            self.sql = sql.into();
            self.parameters = parameters;
            Ok(std::mem::take(&mut self.rows))
        }
        async fn explain(&mut self, _: &str) -> Result<Option<crate::rows::Rec>> {
            panic!("log reads cannot plan user SQL")
        }
    }

    #[tokio::test]
    async fn logged_runs_bind_the_database_and_both_requested_native_bounds() {
        let mut reader = LoggedRead { rows: vec![native_run(Value::NULL)], parameters: vec![], sql: String::new() };
        let (runs, truncated) =
            read_runs(&mut reader, "shop", "2026-11-13 07:00:00", Some("2026-11-13 09:00:00")).await.unwrap();
        assert_eq!(
            reader.parameters,
            [Value::from("shop"), Value::from("2026-11-13 07:00:00"), Value::from("2026-11-13 09:00:00")]
        );
        assert!(reader.sql.contains("start_time >= ? AND start_time < ?"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].ms, 12.345);
        assert_eq!(runs[0].lock_ms, None);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn reaching_the_owned_row_limit_retains_a_truncated_population() {
        let mut reader = LoggedRead {
            rows: (0..MAX_ROWS).map(|_| native_run(Value::UInt(0))).collect(),
            parameters: vec![],
            sql: String::new(),
        };
        let (runs, truncated) =
            read_runs(&mut reader, "shop", "2026-11-13 07:00:00", Some("2026-11-13 09:00:00")).await.unwrap();
        assert_eq!(runs.len(), culprit_core::limits::MAX_RESULT_ROWS);
        assert!(truncated, "reaching the native LIMIT cannot prove the entire requested range was read");
    }

    fn run(at: f64, ms: f64, sql: &str) -> Run {
        Run { at, at_text: iso(at), ms, lock_ms: Some(0.5), rows_sent: 1.0, rows_examined: 100.0, sql: sql.into() }
    }

    #[test]
    fn iso_times() {
        assert_eq!(iso(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_791_734_400.0), "2026-10-11T16:00:00Z");
        assert_eq!(culprit_core::changes::epoch(&iso(1_791_734_461.0)), Some(1_791_734_461.0));
        assert_eq!(sql_time("2026-10-11T07:00:00Z"), "2026-10-11 07:00:00");
    }

    #[test]
    fn runs_group_by_shape_with_percentiles_and_a_timeline() {
        let t0 = 1_791_734_400.0;
        let mut runs: Vec<Run> = (0..40)
            .map(|i| run(t0 + f64::from(i) * 60.0, 100.0 + f64::from(i), &format!("SELECT * FROM t WHERE id = {i}")))
            .collect();
        runs.push(run(t0 + 30.0, 3000.0, "SELECT * FROM big WHERE x LIKE '%a%'"));
        runs.push(run(t0 + 90.0, 1.0, "/* culprit */ SELECT 1"));
        runs.retain(|r| !ignored(&r.sql));
        runs.reverse();
        let c = workload(&runs, &iso(t0), &iso(t0 + 3600.0), Some(0.1), 10, false);
        let w = &c.workload;
        assert_eq!((w.source.as_str(), w.windowed, w.queries.len()), ("slow query log", true, 2));
        assert_eq!(w.total_executions, 41.0);
        let lookup = &w.queries[0];
        assert_eq!(lookup.text, "SELECT * FROM t WHERE id = ?");
        assert_eq!(lookup.executions, 40.0);
        let spread = lookup.spread.as_ref().unwrap();
        let observed = &spread.distributions[0];
        assert!(observed.is_usable());
        assert_eq!(
            (
                spread.min_ms,
                observed.p50.as_ref().and_then(|q| q.upper_ms()),
                observed.p99.as_ref().and_then(|q| q.upper_ms())
            ),
            (Some(100.0), Some(119.0), Some(139.0))
        );
        assert_eq!(observed.population.sample_count(), Some(40));
        assert!(matches!(observed.population, culprit_core::DistributionPopulation::CapturedSamples { .. }));
        assert!(!observed.histogram.is_empty());
        let timeline = w.timeline.as_ref().unwrap();
        assert_eq!(timeline.source, TimelineSource::SlowLog);
        assert_eq!(timeline.executions.iter().sum::<f64>(), 41.0);
        let trend = lookup.trend.as_ref().unwrap();
        assert_eq!(trend.executions.iter().flatten().sum::<f64>(), 40.0);
        assert_eq!(c.samples[0].1, "SELECT * FROM t WHERE id = 39", "the newest run is the sample");
        assert!(c.threshold_note.render().contains("100 ms"));
        assert_eq!(w.min_logged_ms, None, "today's global threshold cannot prove a historical floor");
    }

    #[test]
    fn the_window_is_the_one_read_not_the_first_slow_run() {
        // A week read, with the only slow runs in the last hour: the week is what was watched.
        let now = 1_791_734_400.0;
        let runs = vec![
            run(now - 600.0, 2000.0, "SELECT * FROM t WHERE a = 1"),
            run(now - 1200.0, 2500.0, "SELECT * FROM t WHERE a = 2"),
        ];
        let week = iso(now - 7.0 * 86_400.0);
        let c = workload(&runs, &week, &iso(now), Some(1.0), 10, false);
        assert_eq!(c.workload.window_start.as_deref(), Some(week.as_str()));
        assert!((c.workload.window_hours - 168.0).abs() < 1e-6);
        // A read cut short at the row limit covers only what it reached.
        let c = workload(&runs, &week, &iso(now), Some(1.0), 10, true);
        assert_eq!(c.workload.window_start, Some(iso(now - 1200.0)));
        assert!((c.workload.window_hours - 1200.0 / 3600.0).abs() < 1e-6);
        // A zero threshold still does not establish that every session and statement was logged.
        assert_eq!(workload(&runs, &week, &iso(now), Some(0.0), 10, false).workload.min_logged_ms, None);
    }

    #[test]
    fn periods_compare_runs() {
        let t0 = 1_791_734_400.0;
        let runs = vec![
            run(t0 + 10.0, 100.0, "SELECT a FROM t WHERE id = 1"),
            run(t0 + 20.0, 120.0, "SELECT a FROM t WHERE id = 2"),
            run(t0 + 4000.0, 900.0, "SELECT a FROM t WHERE id = 3"),
        ];
        let baseline = Period { start: iso(t0), end: iso(t0 + 3600.0) };
        let recent = Period { start: iso(t0 + 3600.0), end: iso(t0 + 7200.0) };
        let data = periods(&runs, &baseline, &recent, Some(t0));
        assert_eq!(data.queries.len(), 1);
        let q = &data.queries[0];
        assert_eq!(q.baseline.as_ref().map(|s| (s.executions, s.mean_ms)), Some((2.0, 110.0)));
        assert_eq!(q.recent.as_ref().map(|s| s.max_ms), Some(Some(900.0)));
        assert_eq!(data.baseline_coverage, 1.0);
    }

    // What the slow query log's notes say, word for word: their messages come from locales/en.json.
    #[test]
    fn slow_log_notes_say_what_it_keeps_in_english() {
        let t0 = 1_791_734_400.0;
        let runs = vec![run(t0 + 10.0, 2000.0, "SELECT a FROM t WHERE id = 1")];
        let note = |threshold: Option<f64>| {
            workload(&runs, &iso(t0), &iso(t0 + 3600.0), threshold, 10, false).threshold_note.render()
        };
        assert_eq!(
            note(Some(0.1)),
            "These totals include only runs read from the slow query log. The current long_query_time setting is \
             100 ms; historical session thresholds and other logging filters are not established."
        );
        assert_eq!(
            note(Some(2.5)),
            "These totals include only runs read from the slow query log. The current long_query_time setting is \
             2.5 s; historical session thresholds and other logging filters are not established."
        );
        assert_eq!(
            note(Some(10.0)),
            "These totals include only runs read from the slow query log. The current long_query_time setting is \
             10 s; historical session thresholds and other logging filters are not established."
        );
        assert_eq!(
            note(Some(0.0)),
            "long_query_time is currently 0. These totals include only logged runs; this setting does not establish that every statement was recorded."
        );
        assert_eq!(
            note(None),
            "These totals include only runs read from the slow query log. Historical timing thresholds and other logging filters are not established."
        );
        let baseline = Period { start: iso(t0), end: iso(t0 + 3600.0) };
        let recent = Period { start: iso(t0 + 3600.0), end: iso(t0 + 7200.0) };
        assert_eq!(
            periods(&runs, &baseline, &recent, Some(t0)).notes,
            [
                "Both periods count only runs read from the slow query log; logging filters and thresholds may have differed."
            ]
        );
    }
}
