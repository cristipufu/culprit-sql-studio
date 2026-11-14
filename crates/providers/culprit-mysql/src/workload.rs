//! Statement digests, slow-log fallback, samples and safe estimated plan capture.
use crate::{
    connect::{Connector, Flavor, Scope, TAG},
    fingerprint::{fingerprint, matches_digest},
    plan_json,
    probe::Probe,
    rows::{Rec, explain_one, query, query_one, query_with},
    slowlog,
};
use culprit_core::i18n::{Message, MessageValue};
use culprit_core::{Engine, Plan, PlanKind, QueryStat, Result, SnapshotOptions, Spread, WaitStat, Workload, key};
use mysql_async::Value;
use std::collections::{BTreeSet, HashMap};

pub(crate) struct CollectedWorkload {
    pub(crate) workload: Workload,
    /// Waits the workload source measured itself (the slow query log's lock time), for servers without wait totals.
    pub(crate) waits: Vec<WaitStat>,
    pub(crate) notes: Vec<(&'static str, Message)>,
    /// Why no statement could be planned and what would change that, when none was.
    pub(crate) no_plans: Option<(Message, Option<Message>)>,
}

struct Digest {
    stat: QueryStat,
    sample: Option<String>,
    /// Where `sample` came from, said on the plan made from it.
    sample_from: SampleFrom,
    rows_examined: f64,
}

use culprit_core::PlanSampleSource as SampleFrom;

const PICOS_PER_MS: f64 = 1e9;

use crate::native::runtime_distributions as distributions;

pub(crate) async fn collect_workload(
    conn: &mut impl crate::read::QueryRead,
    connector: &std::sync::Arc<Connector>,
    probe: &Probe,
    options: &SnapshotOptions,
) -> Result<Option<CollectedWorkload>> {
    if !probe.performance_schema {
        return match probe.slow_log_table() {
            true => slow_log_workload(conn, connector, probe, options).await,
            false => Ok(None),
        };
    }
    let available = crate::native::statements::columns(conn).await?;
    let has = |c: &str| available.iter().any(|a| a == c);
    let optional = |c: &str| if has(c) { c.to_string() } else { format!("NULL AS {c}") };
    let [p95, p99] = distributions::summary_projection(&available);
    // Rows examined stands in for "logical reads": MySQL counts rows the storage engine handed to the server, not pages.
    // EXPLAINs and ANALYZEs are Culprit's own planning; `SELECT @@…` digests are driver handshakes, never actionable.
    let sql = format!(
        "SELECT DIGEST AS digest, DIGEST_TEXT AS digest_text, COUNT_STAR AS calls, SUM_TIMER_WAIT AS total_ps,
                MAX_TIMER_WAIT AS max_ps, MIN_TIMER_WAIT AS min_ps, SUM_ROWS_SENT AS rows_sent, SUM_ROWS_EXAMINED AS rows_examined,
                {temporary}, SUM_NO_INDEX_USED AS no_index, {lock},
                {cpu}, {sample}, {p95}, {p99}, {affected},
                DATE_FORMAT(FIRST_SEEN, '%Y-%m-%dT%H:%i:%sZ') AS first_seen,
                DATE_FORMAT(LAST_SEEN, '%Y-%m-%dT%H:%i:%sZ') AS last_seen
           FROM performance_schema.events_statements_summary_by_digest
          WHERE SCHEMA_NAME = ? AND DIGEST IS NOT NULL AND DIGEST_TEXT NOT LIKE 'EXPLAIN %'
            AND DIGEST_TEXT NOT LIKE 'ANALYZE FORMAT%' AND DIGEST_TEXT NOT LIKE 'SHOW ANALYZE%'
            AND DIGEST_TEXT NOT LIKE 'SHOW EXPLAIN%' AND DIGEST_TEXT NOT LIKE 'SELECT @@%'",
        cpu = optional("SUM_CPU_TIME"),
        sample = optional("QUERY_SAMPLE_TEXT"),
        p95 = p95,
        p99 = p99,
        affected = optional("SUM_ROWS_AFFECTED"),
        lock = optional(crate::native::statements::LOCK_TIME_COLUMN),
        temporary = crate::native::statements::temporary_projection(&available),
    );
    let rows = query_with(conn, &sql, vec![Value::from(probe.database.clone())]).await?;
    let all: Vec<Digest> = rows
        .iter()
        .filter(|r| {
            !r.text("query_sample_text").is_some_and(|s| {
                culprit_core::telemetry::StatementOrigin::from_sql(Some(&s), Engine::Mysql).0.is_culprit()
            })
        })
        .map(|r| digest(r, has("SUM_CPU_TIME"), &available))
        .collect::<Result<_>>()?;
    let total_time_ms: f64 = all.iter().map(|d| d.stat.total_time_ms).sum();
    // SUM_CPU_TIME stays 0 unless CPU instrumentation is on; all zeros means unknown, not free.
    let total_cpu_ms: Option<f64> = has("SUM_CPU_TIME")
        .then(|| all.iter().filter_map(|d| d.stat.cpu_time_ms).sum::<f64>())
        .filter(|cpu| *cpu > 0.0);
    let mut all = all;
    if total_cpu_ms.is_none() {
        all.iter_mut().for_each(|d| d.stat.cpu_time_ms = None);
    }
    let total_executions: f64 = all.iter().map(|d| d.stat.executions).sum();
    let query_count = all.len() as f64;
    let window_start = all.iter().filter_map(|d| d.stat.first_seen.clone()).min().or(probe.started_at.clone());
    let window_hours =
        window_start.as_deref().and_then(|start| hours_between(start, &probe.now)).unwrap_or(0.0).max(1.0 / 60.0);

    let mut kept = select_top(all, options.top_queries as usize);
    attach_histograms(conn, &probe.database, &mut kept).await;
    let mut notes = Vec::new();
    if kept
        .iter()
        .filter_map(|digest| digest.stat.spread.as_ref())
        .flat_map(|spread| &spread.distributions)
        .any(|observed| !observed.is_usable())
    {
        notes.push(("workload", culprit_core::message!("provider.mysql.workload.distributions_partial")));
    }
    let mut no_plans = None;
    if !kept.is_empty() && options.plans > 0 {
        let limit = options.plans as usize;
        let planned_part = limit.min(kept.len());
        attach_samples(conn, probe, options, &mut kept[..planned_part]).await;
        let outcome = attach_plans(connector, &probe.database, &mut kept, limit).await;
        if let Some(message) = outcome.note() {
            notes.push(("plans", message));
        }
        if outcome.planned == 0 {
            no_plans = Some(no_plans_reason(conn, probe, has("QUERY_SAMPLE_TEXT")).await);
        }
    }
    let queries = kept.into_iter().map(|d| d.stat).collect();
    let untracked_time_ms = untracked_ms(conn).await;
    if let Some(ms) = untracked_time_ms {
        notes.push(("workload", digests_full(ms)));
    }
    Ok(Some(CollectedWorkload {
        waits: Vec::new(),
        workload: Workload {
            source_label: None,
            source_message: None,
            counter_set: None,
            history_inputs: Vec::new(),
            source: "performance_schema".into(),
            kind: Some(culprit_core::WorkloadKind::PerformanceSchema),
            windowed: false,
            window_start,
            window_end: Some(probe.now.clone()),
            window_hours,
            total_time_ms,
            total_cpu_ms,
            total_executions,
            query_count,
            queries,
            timeline: None,
            untracked_time_ms,
            hourly_time_ms: None,
            min_logged_ms: None,
            log: None,
        },
        notes,
        no_plans,
    }))
}

/// The note on statement time (`ms`) performance_schema couldn't keep per digest.
fn digests_full(ms: f64) -> Message {
    culprit_core::message!("provider.mysql.workload.digests_full", time = MessageValue::Duration(ms))
}

/// Statement time performance_schema couldn't keep per digest once its table was full: the row with no digest. It is
/// the whole server's (it has no schema). `None` when there is none, or it can't be read.
async fn untracked_ms(conn: &mut impl crate::read::QueryRead) -> Option<f64> {
    query_one(
        conn,
        "SELECT SUM_TIMER_WAIT AS total_ps FROM performance_schema.events_statements_summary_by_digest WHERE DIGEST IS NULL",
    )
    .await
    .ok()
    .flatten()
    .and_then(|r| r.number("total_ps"))
    .map(|ps| ps / PICOS_PER_MS)
    .filter(|ms| *ms > 0.0)
}

/// Statements recorded in the slow query log table over the scan window, grouped by shape.
async fn slow_log_workload(
    conn: &mut impl crate::read::QueryRead,
    connector: &std::sync::Arc<Connector>,
    probe: &Probe,
    options: &SnapshotOptions,
) -> Result<Option<CollectedWorkload>> {
    let now = culprit_core::changes::epoch(&probe.now).ok_or_else(crate::native::invalid)?;
    let since = slowlog::iso(now - f64::from(options.window_in_minutes()) * 60.0);
    let (runs, truncated) =
        slowlog::read_runs(conn, &probe.database, &slowlog::sql_time(&since), Some(&slowlog::sql_time(&probe.now)))
            .await?;
    if runs.is_empty() {
        return Ok(None);
    }
    let collected =
        slowlog::workload(&runs, &since, &probe.now, probe.long_query_time, options.top_queries as usize, truncated);
    let samples: HashMap<String, String> = collected.samples.into_iter().collect();
    let mut kept: Vec<Digest> = collected
        .workload
        .queries
        .iter()
        .map(|q| Digest {
            sample: samples.get(&q.id).cloned(),
            sample_from: SampleFrom::SlowLog,
            rows_examined: 0.0,
            stat: q.clone(),
        })
        .collect();
    let mut notes = vec![("workload", collected.threshold_note)];
    let mut no_plans = None;
    if options.plans > 0 {
        let outcome = attach_plans(connector, &probe.database, &mut kept, options.plans as usize).await;
        if let Some(message) = outcome.note() {
            notes.push(("plans", message));
        }
        if outcome.planned == 0 {
            no_plans = Some((no_logged_plans(), None));
        }
    }
    let mut workload = collected.workload;
    workload.queries = kept.into_iter().map(|d| d.stat).collect();
    let waits = slowlog::lock_waits(collected.lock_ms);
    Ok(Some(CollectedWorkload { workload, waits, notes, no_plans }))
}

fn digest(r: &Rec, has_cpu: bool, columns: &BTreeSet<String>) -> Result<Digest> {
    let ms = |key: &str| r.number(key).map(|p| p / PICOS_PER_MS);
    let executions = r.number("calls").unwrap_or(0.0);
    let total = r.number("total_ps").unwrap_or(0.0) / PICOS_PER_MS;
    let digest = r.string("digest");
    let lock_ms = crate::native::statements::lock_time_ms(r)?;
    Ok(Digest {
        stat: QueryStat {
            plan_history: None,
            variant_family: None,
            resolved_write_target: None,
            requests: None,
            id: digest.chars().take(16).collect(),
            query_hash: Some(digest),
            text: r.string("digest_text"),
            parameters: None,
            object_name: None,
            executions,
            total_time_ms: total,
            mean_time_ms: if executions > 0.0 { total / executions } else { 0.0 },
            max_time_ms: r.number("max_ps").map(|p| p / PICOS_PER_MS),
            stddev_time_ms: None,
            cpu_time_ms: if has_cpu { r.number("sum_cpu_time").map(|p| p / PICOS_PER_MS) } else { None },
            rows: r.number("rows_sent"),
            logical_reads: r.number("rows_examined"),
            physical_reads: None,
            temporary_work: crate::native::statements::temporary_work(r, columns)?,
            first_seen: r.text("first_seen"),
            last_seen: r.text("last_seen"),
            plans: Vec::new(),
            plan: None,
            waits: crate::native::statements::lock_waits(lock_ms),
            waits_coverage: None,
            trend: None,
            // Percentiles exist from 8.0.31; the histogram, attached later, from 8.0.19.
            spread: Some(Spread {
                min_ms: ms("min_ps"),
                distributions: distributions::summary(r).into_iter().collect(),
                lock_ms,
            }),
            costs: None,
            // `rows` is what the statement sent back (none for an UPDATE); this is what it changed.
            rows_affected: r.number("sum_rows_affected"),
            plan_id: None,
        },
        sample: r.text("query_sample_text"),
        sample_from: SampleFrom::Digest,
        rows_examined: r.number("rows_examined").unwrap_or(0.0),
    })
}

/// MySQL 8.0.19+ keeps a run-time histogram per digest: enough to tell a long tail from a uniformly slow statement.
/// Older servers have no histogram table; the spread then shows fastest, average and slowest only.
async fn attach_histograms(conn: &mut impl crate::read::QueryRead, database: &str, kept: &mut [Digest]) {
    let digests: Vec<String> = kept.iter().filter_map(|d| d.stat.query_hash.clone()).collect();
    if digests.is_empty() {
        return;
    }
    let mut by_digest =
        match distributions::read_histograms(conn, database, &digests, distributions::MAX_TOTAL_BINS).await {
            Ok(observations) => observations.into_iter().map(|point| (point.digest, point.distribution)).collect(),
            Err(_) => distributions::unavailable(&digests, culprit_core::RuntimeDistributionIssue::ReadUnavailable),
        };
    for d in kept.iter_mut() {
        let Some(observed) = d.stat.query_hash.as_ref().and_then(|h| by_digest.remove(h)) else { continue };
        let spread = d.stat.spread.get_or_insert_with(Spread::default);
        spread.distributions.push(observed);
    }
}

/// Keep the heaviest digests by time, plus the heaviest by executions and rows examined, so chatty and scan-heavy
/// statements are not crowded out. The result is ordered by time, heaviest first.
fn select_top(all: Vec<Digest>, limit: usize) -> Vec<Digest> {
    if limit == 0 || all.is_empty() {
        return Vec::new();
    }
    let ranked = |key: &dyn Fn(&Digest) -> f64| -> Vec<usize> {
        let mut idx: Vec<usize> = (0..all.len()).collect();
        idx.sort_by(|&a, &b| key(&all[b]).total_cmp(&key(&all[a])));
        idx
    };
    let by_time = ranked(&|d| d.stat.total_time_ms);
    let by_calls = ranked(&|d| d.stat.executions);
    let by_examined = ranked(&|d| d.rows_examined);
    let other = (limit / 4).max(1);
    let mut chosen: Vec<usize> = Vec::new();
    let candidates = by_time
        .iter()
        .take(limit.saturating_sub(2 * other).max(1))
        .chain(by_calls.iter().take(other))
        .chain(by_examined.iter().take(other))
        .chain(by_time.iter());
    for &i in candidates {
        if chosen.len() >= limit {
            break;
        }
        if !chosen.contains(&i) {
            chosen.push(i);
        }
    }
    let mut kept: Vec<Digest> =
        all.into_iter().enumerate().filter(|(i, _)| chosen.contains(i)).map(|(_, d)| d).collect();
    kept.sort_by(|a, b| b.stat.total_time_ms.total_cmp(&a.stat.total_time_ms));
    kept
}

fn plannable(sample: &str) -> bool {
    let trimmed = sample.trim_start().trim_start_matches('(').trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let starts = ["select", "with", "table "].iter().any(|k| lower.starts_with(k));
    // A sample cut at performance_schema_max_sql_text_length, or a server-side prepared statement, can't be planned.
    starts && !trimmed.ends_with("...") && !trimmed.contains(" ?") && !trimmed.contains("=?")
}

#[derive(Default)]
struct PlanOutcome {
    planned: usize,
    /// Reads the server kept no text for.
    no_text: usize,
    /// Reads whose text was cut short or prepared with placeholders.
    unusable: usize,
    /// Statements that change data (or aren't queries): Culprit plans reads only.
    not_reads: usize,
    /// Samples whose values matched nothing, so MySQL showed no operators ("no matching row in const table").
    empty: usize,
    failed: usize,
    error: Option<String>,
}

impl PlanOutcome {
    fn note(&self) -> Option<Message> {
        if let Some(error) = &self.error {
            return Some(culprit_core::message!("provider.mysql.plans.no_session", error = error));
        }
        // Each statement that couldn't be planned, by why: (its message, how many).
        let problems: Vec<Message> = [
            (key!("provider.mysql.plans.problem.no_text"), self.no_text),
            (key!("provider.mysql.plans.problem.unusable"), self.unusable),
            (key!("provider.mysql.plans.problem.matched_no_rows"), self.empty),
            (key!("provider.mysql.plans.problem.failed"), self.failed),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| Message::catalog(key, &[("n", n.into()), ("count", MessageValue::Digits(n.to_string()))]))
        .collect();
        // The reasons, each a clause of its own, listed as the language lists them ("a, b, c").
        let problems = problems
            .into_iter()
            .rev()
            .reduce(|rest, first| culprit_core::message!("common.list.more", first = first, rest = rest))?;
        Some(culprit_core::message!(
            "provider.mysql.plans.planned",
            n = self.planned,
            count = MessageValue::Digits(self.planned.to_string()),
            problems = problems
        ))
    }
}

/// Whether a statement is a query Culprit would plan (by its digest text or a sample).
fn reads_shape(text: &str) -> bool {
    let lower = text.trim_start().trim_start_matches('(').trim_start().to_ascii_lowercase();
    ["select", "with", "table "].iter().any(|k| lower.starts_with(k))
}

async fn attach_plans(
    connector: &std::sync::Arc<Connector>,
    database: &str,
    queries: &mut [Digest],
    limit: usize,
) -> PlanOutcome {
    let mut outcome = PlanOutcome::default();
    let mut conn = crate::read::ReadSession::application(connector.clone(), Scope::Database);
    // Initializes/version-checks inside the native owner before selecting flavor-specific setup.
    if let Err(error) = query_one(&mut conn, "SELECT VERSION() AS version").await {
        outcome.error = Some(error.message);
        conn.close().await;
        return outcome;
    }
    if connector.flavor() == Flavor::Mysql {
        let _ = query(&mut conn, "SET SESSION explain_json_format_version = 1").await;
    }
    for digest in queries.iter_mut().take(limit) {
        // Sample text was written by whoever ran the statement: it must lex as exactly one statement, and EXPLAIN goes
        // through a prepared statement, which can't hold a second one.
        let sample = digest.sample.as_deref().filter(|s| plannable(s));
        let Some(sample) = sample.and_then(|s| culprit_core::sql::single_statement(s, Engine::Mysql).ok()) else {
            match (&digest.sample, reads_shape(&digest.stat.text)) {
                (_, false) => outcome.not_reads += 1,
                (None, true) => outcome.no_text += 1,
                (Some(_), true) => outcome.unusable += 1,
            }
            continue;
        };
        let sql = format!("EXPLAIN FORMAT=JSON {sample}");
        match explain_one(&mut conn, &sql).await {
            Ok(Some(row)) => {
                let json = row.text("explain").or_else(|| row.first_text()).unwrap_or_default();
                match sample_plan(&json, sample, database, &digest.stat.text) {
                    // Nothing under the query block: the sample's values matched no row and MySQL stopped there. An
                    // empty diagram would read as "this query does nothing"; say why instead.
                    Ok(plan) if plan.root.children.is_empty() => outcome.empty += 1,
                    Ok(mut plan) => {
                        if let Some(version) = connector.version() {
                            plan.facts_mut().server_version = Some(version.to_string());
                            plan.props.insert("server_version".into(), version.to_string());
                        }
                        // A simple MariaDB plan reads like MySQL's JSON; say which server made it so notes name it.
                        if connector.flavor() == Flavor::Mariadb {
                            plan.facts_mut().flavor = Some("MariaDB".into());
                            plan.props.insert("flavor".into(), "MariaDB".into());
                        }
                        plan.facts.sample_source = Some(digest.sample_from);
                        plan.render_recorded_messages();
                        digest.stat.plan = Some(plan);
                        outcome.planned += 1;
                    }
                    Err(_) => outcome.failed += 1,
                }
            }
            _ => outcome.failed += 1,
        }
    }
    conn.close().await;
    outcome
}

/// The plan of a captured sample, kept without the sample's values. A sample is someone else's real statement (an
/// email, a token, an id in a WHERE clause) and MySQL copies its literals into the plan's conditions and JSON; the
/// report, the UI and coding agents get the digest's text and a plan whose literals are placeholders. LIKE patterns
/// keep their wildcard shape (`'%?'`), so a leading wildcard is still visible. One number is kept: the row count of
/// the sample's LIMIT where the digest writes `LIMIT ?` ([`culprit_core::insights::LIMIT_ROWS`]), the statement's
/// shape rather than its data, so rows returned can be weighed against it.
fn sample_plan(json: &str, sample: &str, database: &str, digest_text: &str) -> Result<Plan> {
    let plan = plan_json::parse(json, PlanKind::Estimated, Some(sample), Some(database))?;
    let mut clean = culprit_core::redact::redact_plan(&plan);
    clean.raw = plan.raw.as_deref().map(|raw| culprit_core::redact::redact_native(raw, Engine::Mysql).into());
    clean.statement = Some(digest_text.to_string());
    if culprit_core::sql::has_limit(digest_text, Engine::Mysql)
        && culprit_core::sql::limit_count(digest_text, Engine::Mysql).is_none()
        && let Some(rows) = culprit_core::sql::limit_count(sample, Engine::Mysql)
    {
        clean.facts_mut().sample_limit_rows = culprit_core::plan_facts::nonnegative(rows);
        clean.props.insert(culprit_core::insights::LIMIT_ROWS.into(), rows.to_string());
    }
    Ok(clean)
}

/// Statement text to plan each digest by. MySQL 8 keeps a sample per digest; MariaDB (and older MySQL) only keep the
/// statements in the history tables, and the slow query log table has real statements of the same shape.
async fn attach_samples(
    conn: &mut impl crate::read::QueryRead,
    probe: &Probe,
    options: &SnapshotOptions,
    kept: &mut [Digest],
) {
    let wanted = |d: &Digest| reads_shape(&d.stat.text) && !d.sample.as_deref().is_some_and(plannable);
    let digests: Vec<String> = kept.iter().filter(|d| wanted(d)).filter_map(|d| d.stat.query_hash.clone()).collect();
    if !digests.is_empty() {
        let marks = vec!["?"; digests.len()].join(", ");
        let mut found: HashMap<String, String> = HashMap::new();
        for table in ["events_statements_history_long", "events_statements_history", "events_statements_current"] {
            let sql = format!(
                "SELECT DIGEST AS digest, SQL_TEXT AS sql_text FROM performance_schema.{table}
                  WHERE CURRENT_SCHEMA = ? AND DIGEST IN ({marks}) AND SQL_TEXT IS NOT NULL
                  ORDER BY TIMER_START DESC"
            );
            let mut params = vec![Value::from(probe.database.clone())];
            params.extend(digests.iter().cloned().map(Value::from));
            for r in query_with(conn, &sql, params).await.unwrap_or_default() {
                if let (Some(digest), Some(text)) = (r.text("digest"), r.text("sql_text"))
                    && !text.contains(TAG)
                    && plannable(&text)
                {
                    found.entry(digest).or_insert(text);
                }
            }
        }
        for d in kept.iter_mut().filter(|d| wanted(d)) {
            if let Some(text) = d.stat.query_hash.as_ref().and_then(|h| found.remove(h)) {
                d.sample = Some(text);
                d.sample_from = SampleFrom::History;
            }
        }
    }
    if probe.slow_log_table() && kept.iter().any(&wanted) {
        let now = culprit_core::changes::epoch(&probe.now).unwrap_or(0.0);
        let since = slowlog::iso(now - f64::from(options.window_in_minutes()) * 60.0);
        if let Ok(runs) = slowlog::read(conn, &probe.database, &slowlog::sql_time(&since), None).await {
            let keyed: Vec<(String, &str)> = runs.iter().map(|r| (fingerprint(&r.sql).key, r.sql.as_str())).collect();
            for d in kept.iter_mut().filter(|d| wanted(d)) {
                if let Some((_, text)) =
                    keyed.iter().find(|(key, text)| plannable(text) && matches_digest(key, &d.stat.text))
                {
                    d.sample = Some(text.to_string());
                    d.sample_from = SampleFrom::SlowLog;
                }
            }
        }
    }
}

/// Why nothing could be planned, and the change that would let Culprit plan statements. `sample_text`: the digest
/// table keeps a sample statement per digest (MySQL 8.0.3+); without it the statement history is the only text.
async fn no_plans_reason(
    conn: &mut impl crate::read::QueryRead,
    probe: &Probe,
    sample_text: bool,
) -> (Message, Option<Message>) {
    // The history tables fill only under the current-statement consumer.
    let off: Vec<String> = query(
        conn,
        "SELECT NAME AS name FROM performance_schema.setup_consumers
          WHERE NAME IN ('events_statements_current', 'events_statements_history_long') AND ENABLED = 'NO'",
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| r.string("name"))
    .collect();
    no_plans_words(probe.flavor, sample_text, !off.is_empty())
}

/// Why no logged statement could be planned.
fn no_logged_plans() -> Message {
    culprit_core::message!("provider.mysql.plans.none_logged")
}

/// [`no_plans_reason`] in words: `consumers_off`, the consumers that keep the latest statements are off.
fn no_plans_words(flavor: Flavor, sample_text: bool, consumers_off: bool) -> (Message, Option<Message>) {
    if !sample_text && consumers_off {
        let names = ["events_statements_current", "events_statements_history_long"];
        let settings: Vec<String> = names.iter().map(|n| format!("performance_schema_consumer_{n} = ON")).collect();
        // MySQL 8.0 keeps a sample statement per digest; MariaDB never does.
        let why = match flavor {
            Flavor::Mariadb => culprit_core::message!("provider.mysql.plans.no_text.mariadb"),
            Flavor::Mysql => culprit_core::message!("provider.mysql.plans.no_text.mysql"),
        };
        return (
            why,
            Some(culprit_core::message!(
                "provider.mysql.plans.no_text_fix",
                sql = format!(
                    "UPDATE performance_schema.setup_consumers SET ENABLED = 'YES' WHERE NAME IN ({});",
                    names.map(|n| format!("'{n}'")).join(", ")
                ),
                config = Message::list(settings.into_iter().map(Message::from).collect())
            )),
        );
    }
    (
        culprit_core::message!("provider.mysql.plans.none_planned"),
        (!sample_text).then(|| {
            culprit_core::message!(
                "provider.mysql.plans.history_only_hint",
                server = flavor.name(),
                sql = "SET GLOBAL slow_query_log = ON, GLOBAL log_output = 'FILE,TABLE';"
            )
        }),
    )
}

/// Hours between two RFC 3339 UTC timestamps.
fn hours_between(start: &str, end: &str) -> Option<f64> {
    Some((epoch(end)? - epoch(start)?) / 3600.0)
}

fn epoch(text: &str) -> Option<f64> {
    let (date, time) = text.trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':').map(|p| p.parse::<f64>());
    let (h, mi, s) = (t.next()?.ok()?, t.next()?.ok()?, t.next().unwrap_or(Ok(0.0)).ok()?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days as f64 * 86_400.0 + h * 3600.0 + mi * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_digest(lock_time: Value) -> Rec {
        native_digest_with_temporary(lock_time, None)
    }

    fn native_digest_with_temporary(lock_time: Value, temporary: Option<[Value; 3]>) -> Rec {
        let mut values = vec![
            ("digest", Value::from("0123456789abcdef0123456789abcdef")),
            ("schema_name", Value::from("shop")),
            ("observed_at", Value::from("2026-11-13T08:00:00.123456Z")),
            ("calls", Value::UInt(2)),
            ("COUNT_STAR", Value::UInt(2)),
            ("total_ps", Value::UInt(1_000_000_000)),
            ("SUM_TIMER_WAIT", Value::UInt(1_000_000_000)),
            (crate::native::statements::LOCK_TIME_COLUMN, lock_time),
        ];
        if let Some([created, disk, passes]) = temporary {
            values.extend([
                ("SUM_CREATED_TMP_TABLES", created),
                ("SUM_CREATED_TMP_DISK_TABLES", disk),
                ("SUM_SORT_MERGE_PASSES", passes),
            ]);
        }
        Rec::from_values(values)
    }

    fn temporary_layout() -> crate::native::statements::Layout {
        crate::native::statements::Layout {
            columns: [
                "SCHEMA_NAME",
                "DIGEST",
                "DIGEST_TEXT",
                "COUNT_STAR",
                "SUM_TIMER_WAIT",
                "SUM_CREATED_TMP_TABLES",
                "SUM_CREATED_TMP_DISK_TABLES",
                "SUM_SORT_MERGE_PASSES",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            cpu_enabled: false,
            digest_enabled: true,
            overflow_calls: 0,
        }
    }

    #[test]
    fn scan_and_export_preserve_exact_native_table_and_sort_counts_without_converting_them_to_bytes_or_time() {
        use culprit_core::{ResetMarker, StatementTemporaryWork, telemetry::*};
        let row = native_digest_with_temporary(
            Value::NULL,
            Some([Value::UInt(u64::MAX), Value::UInt(0), Value::from("9007199254740993")]),
        );
        let layout = temporary_layout();
        let q = super::digest(&row, false, &layout.columns).unwrap().stat;
        assert_eq!(
            q.temporary_work,
            Some(StatementTemporaryWork::TableCreation {
                source: crate::native::statements::SOURCE.into(),
                created_tables: Some(u64::MAX.to_string()),
                disk_tables: Some("0".into()),
                sort_merge_passes: Some("9007199254740993".into()),
            })
        );
        let exported = crate::native::statements::counters(&row, &layout, "shop").unwrap();
        assert_eq!(exported.reset, ResetMarker::Unknown, "the server process is not the digest counter generation");
        for (key, expected) in [
            (RuntimeMetricKey::TemporaryTables, u64::MAX.to_string()),
            (RuntimeMetricKey::TemporaryDiskTables, "0".into()),
            (RuntimeMetricKey::SortMergePasses, "9007199254740993".into()),
        ] {
            let counter = exported.totals.iter().find(|counter| counter.key == key).unwrap();
            assert_eq!(counter.unit, RuntimeMetricUnit::Count);
            assert_eq!(counter.total, MeasurementValue::Integer(expected));
        }
        assert!(q.costs.is_none(), "a count does not establish spill time or allocation size");
    }

    #[test]
    fn nullable_and_unavailable_temp_fields_stay_distinct_from_native_zero_in_scan_and_export() {
        use culprit_core::{StatementTemporaryWork, telemetry::*};
        let layout = temporary_layout();
        let row = native_digest_with_temporary(Value::NULL, Some([Value::UInt(0), Value::NULL, Value::NULL]));
        let q = super::digest(&row, false, &layout.columns).unwrap().stat;
        assert_eq!(
            q.temporary_work,
            Some(StatementTemporaryWork::TableCreation {
                source: crate::native::statements::SOURCE.into(),
                created_tables: Some("0".into()),
                disk_tables: None,
                sort_merge_passes: None,
            })
        );
        let exported = crate::native::statements::counters(&row, &layout, "shop").unwrap();
        assert!(exported.totals.iter().any(|counter| counter.key == RuntimeMetricKey::TemporaryTables
            && counter.total == MeasurementValue::Integer("0".into())));
        assert!(!exported.totals.iter().any(|counter| matches!(
            counter.key,
            RuntimeMetricKey::TemporaryDiskTables | RuntimeMetricKey::SortMergePasses
        )));

        let unknown = native_digest_with_temporary(Value::NULL, Some([Value::NULL, Value::NULL, Value::NULL]));
        assert!(super::digest(&unknown, false, &layout.columns).unwrap().stat.temporary_work.is_none());
        let mut unsupported = layout;
        unsupported
            .columns
            .retain(|column| !column.starts_with("SUM_CREATED_TMP_") && column != "SUM_SORT_MERGE_PASSES");
        let no_fields = native_digest(Value::NULL);
        assert!(super::digest(&no_fields, false, &unsupported.columns).unwrap().stat.temporary_work.is_none());
        assert!(crate::native::statements::counters(&no_fields, &unsupported, "shop").unwrap().totals.iter().all(
            |counter| !matches!(
                counter.key,
                RuntimeMetricKey::TemporaryTables
                    | RuntimeMetricKey::TemporaryDiskTables
                    | RuntimeMetricKey::SortMergePasses
            )
        ));
    }

    #[test]
    fn malformed_or_missing_selected_native_temp_fields_cannot_become_zero_or_partial_success() {
        let layout = temporary_layout();
        for value in
            [Value::Int(-1), Value::Double(3.0), Value::from("not-a-counter"), Value::from("18446744073709551616")]
        {
            let row = native_digest_with_temporary(Value::NULL, Some([Value::UInt(1), value, Value::UInt(0)]));
            assert!(super::digest(&row, false, &layout.columns).is_err());
            assert!(crate::native::statements::counters(&row, &layout, "shop").is_err());
        }
        let missing = native_digest(Value::NULL);
        assert!(super::digest(&missing, false, &layout.columns).is_err());
        assert!(crate::native::statements::counters(&missing, &layout, "shop").is_err());
    }

    #[test]
    fn scan_and_counter_projections_share_native_temp_columns_and_unsupported_columns_are_explicitly_null() {
        use crate::native::statements;
        let mut layout = temporary_layout();
        let columns = statements::temporary_projection(&layout.columns);
        assert_eq!(columns, "SUM_CREATED_TMP_TABLES,SUM_CREATED_TMP_DISK_TABLES,SUM_SORT_MERGE_PASSES");
        let counters = layout.query(false, 129, 4096).unwrap();
        for column in columns.split(',') {
            assert!(counters.contains(column));
        }
        assert!(counters.ends_with("WHERE SCHEMA_NAME=? AND DIGEST IS NOT NULL LIMIT 129"));
        layout.columns.remove("SUM_CREATED_TMP_DISK_TABLES");
        assert_eq!(
            statements::temporary_projection(&layout.columns),
            "SUM_CREATED_TMP_TABLES,NULL AS SUM_CREATED_TMP_DISK_TABLES,SUM_SORT_MERGE_PASSES"
        );
        assert!(!layout.query(false, 129, 4096).unwrap().contains("SUM_CREATED_TMP_DISK_TABLES"));
    }

    #[test]
    fn scan_and_export_preserve_the_same_native_lock_counter_without_complete_lock_coverage() {
        use crate::native::statements::{self, LOCK_TIME_COLUMN, LOCK_TIME_METRIC};
        use culprit_core::telemetry::{MeasurementValue, RuntimeMetricKey, RuntimeMetricUnit, WaitClass};
        let raw = 9_007_199_254_740_993_u64;
        let row = native_digest(Value::UInt(raw));
        let q = super::digest(&row, false, &BTreeSet::new()).unwrap().stat;
        let layout = statements::Layout {
            columns: ["SCHEMA_NAME", "DIGEST", "DIGEST_TEXT", "COUNT_STAR", "SUM_TIMER_WAIT", LOCK_TIME_COLUMN]
                .into_iter()
                .map(str::to_string)
                .collect(),
            cpu_enabled: false,
            digest_enabled: true,
            overflow_calls: 0,
        };
        let exported = statements::counters(&row, &layout, "shop").unwrap();
        let counter = exported
            .totals
            .iter()
            .find(|counter| counter.key == RuntimeMetricKey::Native(LOCK_TIME_METRIC.into()))
            .unwrap();
        assert_eq!(counter.unit, RuntimeMetricUnit::Picoseconds);
        assert_eq!(
            counter.total,
            MeasurementValue::Integer(raw.to_string()),
            "export never rounds a native counter through f64"
        );
        assert_eq!(q.spread.as_ref().unwrap().lock_ms, Some(raw as f64 / PICOS_PER_MS));
        assert_eq!(
            q.waits[0].wait_type.as_deref(),
            Some("performance_schema.events_statements_summary_by_digest.SUM_LOCK_TIME")
        );
        assert_eq!(q.waits[0].wait_ms, raw as f64 / PICOS_PER_MS, "do not clamp a native subtotal to elapsed time");
        assert_eq!(q.waits_coverage, None);
        assert_eq!(q.measured_wait_ms(WaitClass::Lock), None);
    }

    #[test]
    fn unknown_and_zero_native_lock_timers_do_not_establish_absent_row_lock_waits() {
        use culprit_core::telemetry::WaitClass;
        for (native, expected) in [(Value::NULL, None), (Value::UInt(0), Some(0.0))] {
            let q = super::digest(&native_digest(native), false, &BTreeSet::new()).unwrap().stat;
            assert_eq!(q.spread.as_ref().unwrap().lock_ms, expected);
            assert!(q.waits.is_empty());
            assert_eq!(q.measured_wait_ms(WaitClass::Lock), None);
        }
        for malformed in
            [Value::Int(-1), Value::Double(2.0), Value::from("not-a-counter"), Value::from("18446744073709551616")]
        {
            assert!(super::digest(&native_digest(malformed), false, &BTreeSet::new()).is_err());
        }
        assert!(super::digest(&Rec::from_values(vec![]), false, &BTreeSet::new()).is_err());
    }

    fn digest(id: &str, time: f64, calls: f64, examined: f64) -> Digest {
        Digest {
            stat: QueryStat {
                plan_history: None,
                variant_family: None,
                resolved_write_target: None,
                requests: None,
                id: id.into(),
                query_hash: None,
                text: String::new(),
                parameters: None,
                object_name: None,
                executions: calls,
                total_time_ms: time,
                mean_time_ms: 0.0,
                max_time_ms: None,
                stddev_time_ms: None,
                cpu_time_ms: None,
                rows: None,
                logical_reads: Some(examined),
                physical_reads: None,
                temporary_work: None,
                first_seen: None,
                last_seen: None,
                plans: Vec::new(),
                plan: None,
                waits: Vec::new(),
                waits_coverage: None,
                trend: None,
                spread: None,
                costs: None,
                rows_affected: None,
                plan_id: None,
            },
            sample: None,
            sample_from: SampleFrom::Digest,
            rows_examined: examined,
        }
    }

    fn no_plans_said(flavor: Flavor, sample_text: bool, consumers_off: bool) -> (String, Option<String>) {
        let (reason, fix) = no_plans_words(flavor, sample_text, consumers_off);
        (reason.render(), fix.map(|message| message.render()))
    }

    #[test]
    fn a_plan_says_which_run_it_was_planned_from() {
        // performance_schema keeps a digest's slowest recent run as its sample: its values may be an outlier's.
        assert!(SampleFrom::Digest.sentence().contains("slowest recent run"));
        assert!(SampleFrom::SlowLog.sentence().contains("a slow run"));
        assert!(!SampleFrom::History.sentence().contains("outlier"));
    }

    // What the workload's notes and plans say, word for word: their messages come from locales/en.json.
    #[test]
    fn workload_notes_say_what_was_planned_in_english() {
        assert_eq!(
            SampleFrom::Digest.sentence(),
            "Planned from the sample performance_schema keeps for this statement: its slowest recent run, so the values \
             may be an outlier's rather than a typical run's."
        );
        assert_eq!(
            SampleFrom::History.sentence(),
            "Planned from the latest run in performance_schema's statement history."
        );
        assert_eq!(
            SampleFrom::SlowLog.sentence(),
            "Planned from a run in the slow query log: a slow run, so the values may be an outlier's rather than a \
             typical run's."
        );
        let note = |outcome: PlanOutcome| outcome.note().map(|message| message.render());
        assert_eq!(
            note(PlanOutcome { error: Some("Nothing is listening on db:3306".into()), ..Default::default() })
                .as_deref(),
            Some("Could not open a session to plan statements: Nothing is listening on db:3306")
        );
        assert_eq!(
            note(PlanOutcome { planned: 1, unusable: 1, ..Default::default() }).as_deref(),
            Some("Planned 1 query; 1 had only truncated or server-side prepared text.")
        );
        assert_eq!(
            note(PlanOutcome { planned: 0, empty: 1, ..Default::default() }).as_deref(),
            Some("Planned 0 queries; 1 was sampled for values that matched no rows, so the server showed no plan.")
        );
        assert_eq!(
            note(PlanOutcome { planned: 4, no_text: 1, unusable: 2, empty: 3, failed: 5, not_reads: 6, error: None })
                .as_deref(),
            Some(
                "Planned 4 queries; 1 had no recorded statement text, 2 had only truncated or server-side prepared \
                 text, 3 were sampled for values that matched no rows, so the server showed no plan, 5 could not be \
                 explained."
            )
        );
        assert_eq!(
            note(PlanOutcome { planned: 2, failed: 1, ..Default::default() }).as_deref(),
            Some("Planned 2 queries; 1 could not be explained.")
        );
        assert_eq!(
            digests_full(1_500.0).render(),
            "performance_schema's digest table is full (performance_schema_digests_size), so 1.5 s of statement time \
             across the server went into one catch-all row and isn't in these totals: shares are of the time it could \
             attribute."
        );
        assert_eq!(no_logged_plans().render(), "No logged statement could be planned.");
        let consumers = "Run UPDATE performance_schema.setup_consumers SET ENABLED = 'YES' WHERE NAME IN \
                         ('events_statements_current', 'events_statements_history_long'); to keep the latest \
                         statements (no restart needed; add performance_schema_consumer_events_statements_current = ON \
                         and performance_schema_consumer_events_statements_history_long = ON to the server \
                         configuration to keep them after a restart). Or open a statement in the editor and Explain it \
                         there.";
        assert_eq!(
            no_plans_said(Flavor::Mariadb, false, true),
            (
                "MariaDB keeps statement digests but not the statements themselves, so there is no text to plan."
                    .into(),
                Some(consumers.into())
            )
        );
        assert_eq!(
            no_plans_said(Flavor::Mysql, false, true),
            (
                "This MySQL keeps statement digests but no sample statement for each (MySQL 8.0 added one), so there is \
                 no text to plan."
                    .into(),
                Some(consumers.into())
            )
        );
        let none = "None of the recorded statements could be planned: their text was truncated, prepared with \
                    placeholders, or no longer in the statement history.";
        assert_eq!(
            no_plans_said(Flavor::Mariadb, false, false),
            (
                none.into(),
                Some(
                    "MariaDB's statement history keeps only the latest statements \
                     (performance_schema_events_statements_history_long_size). Logging slow statements to the \
                     mysql.slow_log table gives Culprit lasting samples: SET GLOBAL slow_query_log = ON, GLOBAL \
                     log_output = 'FILE,TABLE';"
                        .into()
                )
            )
        );
        assert_eq!(no_plans_said(Flavor::Mysql, true, true), (none.into(), None));
    }

    #[test]
    fn plan_notes_tell_missing_text_from_writes() {
        let outcome = PlanOutcome { planned: 2, no_text: 3, not_reads: 4, ..Default::default() };
        assert_eq!(
            outcome.note().map(|note| note.render()).as_deref(),
            Some("Planned 2 queries; 3 had no recorded statement text.")
        );
        assert_eq!(PlanOutcome { planned: 5, not_reads: 2, ..Default::default() }.note(), None);
        assert!(reads_shape(" (SELECT 1)") && !reads_shape("UPDATE t SET a = 1"));
    }

    #[test]
    fn keeps_chatty_and_scan_heavy_digests() {
        let mut all: Vec<Digest> = (0..20).map(|i| digest(&format!("t{i}"), 1000.0 - f64::from(i), 1.0, 1.0)).collect();
        all.push(digest("chatty", 1.0, 1e6, 1.0));
        all.push(digest("scanner", 2.0, 1.0, 1e9));
        let kept = select_top(all, 8);
        let ids: Vec<&str> = kept.iter().map(|d| d.stat.id.as_str()).collect();
        assert_eq!(kept.len(), 8);
        assert!(ids.contains(&"chatty") && ids.contains(&"scanner") && ids.contains(&"t0"));
        assert_eq!(ids[0], "t0", "kept digests are ordered by time");
    }

    #[test]
    fn sample_plans_keep_no_literal_values() {
        let json = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"1.20"},"table":{"table_name":"customers","access_type":"ALL","rows_examined_per_scan":100,"rows_produced_per_join":10,"filtered":"10.00","cost_info":{"read_cost":"1.00","eval_cost":"0.20","prefix_cost":"1.20","data_read_per_join":"1K"},"used_columns":["id","email"],"attached_condition":"((`shop`.`customers`.`email` = 'alice@example.com') and (`shop`.`customers`.`note` like '%vip%'))"}}}"#;
        let sample = "SELECT id FROM customers WHERE email = 'alice@example.com' AND note LIKE '%vip%'";
        let plan =
            sample_plan(json, sample, "shop", "SELECT `id` FROM `customers` WHERE `email` = ? AND `note` LIKE ?")
                .unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        assert!(!text.contains("alice"), "{text}");
        assert!(!text.contains("vip"), "{text}");
        assert!(text.contains("'%?%'"), "the LIKE pattern keeps its shape: {text}");
        assert_eq!(plan.statement.as_deref(), Some("SELECT `id` FROM `customers` WHERE `email` = ? AND `note` LIKE ?"));
        assert!(!plan.props.contains_key(culprit_core::insights::LIMIT_ROWS), "no LIMIT, no count");
    }

    #[test]
    fn a_sample_keeps_the_count_its_digests_limit_hides() {
        let json = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"50000.20"},"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":500000,"rows_produced_per_join":50000,"filtered":"10.00","cost_info":{"read_cost":"45000.00","eval_cost":"5000.20","prefix_cost":"50000.20","data_read_per_join":"106M"},"used_columns":["id","promo_code"],"attached_condition":"(`shop`.`orders`.`promo_code` = 'SPRING-7')"}}}"#;
        let sample = "SELECT id FROM orders WHERE promo_code = 'SPRING-7' LIMIT 50";
        let digest = "SELECT `id` FROM `orders` WHERE `promo_code` = ? LIMIT ?";
        let plan = sample_plan(json, sample, "shop", digest).unwrap();
        assert_eq!(plan.props.get(culprit_core::insights::LIMIT_ROWS).map(String::as_str), Some("50"));
        // Redacting the plan again (an export, a share) keeps it: it is a number, not a value from the data.
        let again = culprit_core::redact::redact_plan(&plan);
        assert_eq!(again.props.get(culprit_core::insights::LIMIT_ROWS).map(String::as_str), Some("50"));
        assert!(!serde_json::to_string(&again).unwrap().contains("SPRING"));
        // A LIMIT written inside the sample's literal is the application's data, not the statement's shape: it never
        // reaches the plan, which redaction would otherwise leave alone.
        let note = "SELECT id FROM orders WHERE promo_code = 'LIMIT 4242' LIMIT ?";
        let plan = sample_plan(json, note, "shop", digest).unwrap();
        assert!(!plan.props.contains_key(culprit_core::insights::LIMIT_ROWS), "{:?}", plan.props);
        assert!(!serde_json::to_string(&plan).unwrap().contains("4242"));
    }

    #[test]
    fn samples_that_matched_nothing_are_explained() {
        let json = r#"{"query_block":{"select_id":1,"message":"no matching row in const table"}}"#;
        let plan =
            sample_plan(json, "SELECT * FROM orders WHERE id = 0", "shop", "SELECT * FROM `orders` WHERE `id` = ?")
                .unwrap();
        assert!(plan.root.children.is_empty(), "the parser keeps the empty block; attach_plans skips it");
        let outcome = PlanOutcome { planned: 2, empty: 1, ..Default::default() };
        let note = outcome.note().unwrap().render();
        assert!(note.contains("matched no rows"), "{note}");
    }

    #[test]
    fn samples_that_cannot_be_planned_are_skipped() {
        assert!(plannable("SELECT id FROM orders WHERE id = 7"));
        assert!(plannable("  (SELECT 1) UNION (SELECT 2)"));
        assert!(!plannable("SELECT id FROM orders WHERE id = ?"));
        assert!(!plannable("SELECT a, b, c FROM t WHERE x IN (1, 2, ..."));
        assert!(!plannable("UPDATE orders SET x = 1"));
    }

    #[test]
    fn windows_are_measured_in_hours() {
        assert_eq!(hours_between("2026-10-11T06:00:00Z", "2026-10-11T07:30:00Z"), Some(1.5));
        assert_eq!(crate::pressure::category("wait/io/file/innodb/innodb_data_file"), "IO");
        assert_eq!(crate::pressure::category("wait/lock/metadata/sql/mdl"), "Lock");
    }
}
