//! The `log-volume` check: Statements that write a large share of the WAL (PostgreSQL 13+) or transaction log (SQL Server 2017+), or many kilobytes of it per row changed. Log volume drives replication lag, backup size and checkpoints. MySQL keeps no redo bytes per statement.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};

pub fn log_volume_info() -> CheckInfo {
    CheckInfo {
        id: "log-volume".into(),
        title: t!("check.log_volume.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres],
        requires: vec![],
        description: t!("check.log_volume.info.description"),
    }
}

check!(
    LogVolumeCheck: log_volume_info,
    decide = log_writes,
    render = log_volume_finding,
    case<'s> = LogWrites<'s>,
    evidence = &[Evidence::Query(QueryMeasure::LogBytes)],
);

/// A write that makes a large share of the log, or many kilobytes of it per row, with a volume a day behind it.
pub struct LogWrites<'s> {
    q: &'s QueryStat,
    bytes: f64,
    share: f64,
    per_row: Option<f64>,
    /// What full-page images are said to be: their share of the log (wal_compression known off), or their count.
    images: Images,
    /// The table it changes, when it keeps an index nobody reads.
    unread_on: Option<&'s TableStat>,
    fix: LogFix<'s>,
    severity: Severity,
}

/// The full-page images in a statement's WAL (PostgreSQL counts them, not their bytes), as the summary says them.
#[derive(Clone, Copy)]
pub enum Images {
    /// Up to this share of the log, with wal_compression off: 8 KB each is an upper bound and close to the truth.
    Share(f64),
    /// This many records, half or more of the bytes at 8 KB each; compressed or not.
    Count {
        count: f64,
        compressed: bool,
    },
    None,
}

/// The change that writes less log.
#[derive(Clone, Copy)]
pub enum LogFix<'s> {
    /// The table it changes has rows of mostly large values, logged in full: its average row, and their share.
    LargeValues {
        table: &'s TableStat,
        average: f64,
        share: f64,
    },
    /// PostgreSQL: touch each page once per checkpoint, and what to do about full-page images.
    Wal(ImagesFix),
    /// SQL Server: an insert logs the rows it adds.
    Insert,
    Rewrite,
}

#[derive(Clone, Copy)]
pub enum ImagesFix {
    /// They dominate and wal_compression is off: turn it on.
    Compress,
    /// wal_compression already shrinks them.
    Compressed,
    /// It isn't known: both levers, conditionally.
    Unknown,
}

fn log_writes(snapshot: &Snapshot) -> Vec<LogWrites<'_>> {
    let pg = snapshot.server.engine == Engine::Postgres;
    let engine = snapshot.server.engine;
    let mut cases: Vec<(LogWrites<'_>, f64)> = app_queries(snapshot)
        .filter(|q| is_write(q))
        .filter_map(|q| {
            let costs = q.costs.as_ref()?;
            let bytes = costs.log_bytes?;
            let share = costs.log_share.unwrap_or(0.0);
            let rows = q.rows.unwrap_or(0.0);
            // Bytes per row whenever it changed rows: a few hundred rows of large values say as much as a million.
            let per_row = (rows > 0.0).then(|| bytes / rows);
            // A share of the log, or many kilobytes a row, is a threshold only with a volume a day behind it: 30% of a
            // quiet hour is nothing, and neither is 3 GB over three years.
            let heavy_share = share >= 0.3 && window_at_least(snapshot, q, bytes, GB);
            let heavy_rows = per_row.is_some_and(|b| b >= 4_096.0) && window_at_least(snapshot, q, bytes, 100.0 * MB);
            (heavy_share || heavy_rows).then_some((q, costs, bytes, share, per_row))
        })
        .map(|(q, costs, bytes, share, per_row)| {
            // pg_stat_statements counts full-page images, not their bytes. A whole page is 8 KB at most: PostgreSQL
            // leaves out the empty middle of each page, and wal_compression shrinks the rest, so 8 KB each is an upper
            // bound, and close to the truth only with compression off.
            // wal_compression is off, on, or a method (pglz, lz4, zstd from 15).
            let compression = pg
                .then(|| snapshot.setting("wal_compression"))
                .flatten()
                .map(|s| !matches!(s.value.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no"));
            let image_count = costs.log_full_pages.filter(|n| *n > 0.0);
            let image_share =
                image_count.filter(|_| compression == Some(false)).map(|n| (n * 8_192.0 / bytes).min(1.0));
            let images = match (image_share, image_count) {
                (Some(s), _) if s >= 0.5 => Images::Share(s),
                (None, Some(n)) if pg && n * 8_192.0 >= 0.5 * bytes => {
                    Images::Count { count: n, compressed: compression == Some(true) }
                }
                _ => Images::None,
            };
            let busy = share >= 0.5 && window_at_least(snapshot, q, bytes, GB);
            // Wide rows of log are Medium with a volume behind them: 100 MB a day, not 100 MB over a month.
            let wide = per_row.is_some_and(|b| b >= 8_192.0) && window_at_least(snapshot, q, bytes, 100.0 * MB);
            let severity = if busy || wide { Severity::Medium } else { Severity::Low };
            // Ranked by the log it writes a day, not by the statement's share of database time: a quarter of a quiet
            // database's time spent writing a few megabytes of log a day is no log problem.
            let a_day = window_rate_per_day(snapshot, q, bytes).unwrap_or(0.0);
            let score = base_score(severity) + (a_day / MB).max(1.0).log10().min(4.0) * 3.0;
            // The table it changes: whether its rows are mostly large values (then the log is the data itself) and
            // whether it keeps an index nobody reads (then dropping it is advice, not a guess).
            let target = write_targets(q, engine)
                .into_iter()
                .find_map(|o| snapshot.tables.iter().find(|t| names_table(&o, &t.schema, &t.name)));
            let unread = target.is_some_and(|t| {
                snapshot.indexes_on(&t.schema, &t.name).any(|i| crate::indexes::is_unused(snapshot, i))
            });
            let large = target.and_then(|t| large_values(t).map(|(average, share)| (t, average, share)));
            let fix = if let Some((table, average, share)) = large {
                // Large values are logged in full: batching or dropping indexes can't shrink that (large-rows' fix can).
                LogFix::LargeValues { table, average, share }
            } else if pg {
                // wal_compression only where it is known to be off; on or unknown, the checkpoint interval is the lever.
                LogFix::Wal(match (image_share.filter(|s| *s >= 0.5), compression) {
                    (Some(_), _) => ImagesFix::Compress,
                    (None, Some(true)) => ImagesFix::Compressed,
                    (None, _) => ImagesFix::Unknown,
                })
            } else if first_verb(main_statement(&q.text)).is_some_and(|(_, v)| v == "insert") {
                // An insert logs the rows it adds: nothing in them is "unchanged columns" to leave out.
                LogFix::Insert
            } else {
                LogFix::Rewrite
            };
            let case =
                LogWrites { q, bytes, share, per_row, images, unread_on: target.filter(|_| unread), fix, severity };
            (case, score)
        })
        .collect();
    cases.sort_by(|a, b| b.1.total_cmp(&a.1));
    cases.truncate(5);
    cases.into_iter().map(|(case, _)| case).collect()
}

fn log_volume_finding(snapshot: &Snapshot, w: LogWrites<'_>) -> Finding {
    let LogWrites { q, bytes, share, per_row, .. } = w;
    let pg = snapshot.server.engine == Engine::Postgres;
    // The log by its engine's name, as the messages' `select` reads it: WAL, or the transaction log.
    let log = if pg { "wal" } else { "transaction_log" };
    let mut f = finding("log-volume", &q.id, Category::Queries, subject(SubjectKind::Query, &q.id));
    let handle = capitalize(&query_handle(q));
    // A statement that stopped running is told in the past.
    let tense = tense(snapshot, &[q]);
    f.title = match per_row {
        Some(b) if b >= 4_096.0 => {
            t!("check.log_volume.title.per_row", handle = &handle, tense = tense, bytes = format::bytes(b), log = log)
        }
        // The share is of what statements wrote: autovacuum's and checkpoints' WAL isn't in it.
        _ => t!(
            "check.log_volume.title.share",
            handle = &handle,
            tense = tense,
            share = format::percent(share),
            log = log
        ),
    };
    let (written, ran, of_all) = (format::bytes(bytes), ran_phrase(snapshot, q), format::percent(share));
    let mut summary = vec![match per_row {
        Some(b) => t!(
            "check.log_volume.summary.wrote_per_row",
            bytes = written,
            log = log,
            ran = ran,
            share = of_all,
            per_row = format::bytes(b)
        ),
        None => t!("check.log_volume.summary.wrote", bytes = written, log = log, ran = ran, share = of_all),
    }];
    match w.images {
        Images::Share(s) => summary.push(t!("check.log_volume.summary.images_share", share = format::percent(s))),
        Images::Count { count, compressed: false } => {
            summary.push(t!("check.log_volume.summary.images", count = format::count(count)))
        }
        Images::Count { count, compressed: true } => {
            summary.push(t!("check.log_volume.summary.images_compressed", count = format::count(count)))
        }
        Images::None => {}
    }
    f.summary = summary.join(" ");
    f.severity = w.severity;
    with_query(snapshot, &mut f, q);
    let a_day = window_rate_per_day(snapshot, q, bytes).unwrap_or(0.0);
    f.impact.score = base_score(f.severity) + (a_day / MB).max(1.0).log10().min(4.0) * 3.0;
    f.impact.set_label(t!("check.log_volume.impact", bytes = format::bytes(bytes), log = log));
    f.confidence = Confidence::High;
    f.metrics = vec![
        Metric::said(culprit_core::key!("check.log_volume.metric.written"), bytes, Unit::Bytes),
        Metric::said(culprit_core::key!("check.log_volume.metric.share"), share, Unit::Ratio),
    ];
    f.facts.log_write = Some(culprit_core::FindingLogWrite { bytes, per_row });
    if let Some(b) = per_row {
        f.metrics.push(Metric::said(culprit_core::key!("check.log_volume.metric.per_row"), b, Unit::Bytes));
    }
    let drop_unread = w.unread_on.map(|t| t!("check.log_volume.fix.drop_unread", log = log, table = &t.name));
    let mut fix = Vec::new();
    let mut sql = None;
    match w.fix {
        LogFix::LargeValues { table, average, share } => {
            fix.push(t!(
                "check.log_volume.fix.large_values",
                table = &table.name,
                average = format::bytes(average),
                share = share_words(share),
                log = log
            ));
            fix.push(t!("check.large_rows.fix.compress", engine = culprit_core::wire_name(&snapshot.server.engine)));
            fix.extend(drop_unread);
        }
        LogFix::Wal(images) => {
            fix.push(t!("check.log_volume.fix.batch_pages"));
            fix.extend(drop_unread);
            fix.push(match images {
                ImagesFix::Compress => {
                    let change = setting_change(&snapshot.server, "wal_compression", "on");
                    sql = change.sql;
                    t!("check.log_volume.fix.compress_images", how = &change.how)
                }
                ImagesFix::Compressed => t!("check.log_volume.fix.images_compressed"),
                ImagesFix::Unknown => t!("check.log_volume.fix.images_unknown"),
            });
        }
        LogFix::Insert => {
            fix.push(t!("check.log_volume.fix.insert", log = log));
            fix.extend(drop_unread);
        }
        LogFix::Rewrite => {
            fix.push(t!("check.log_volume.fix.rewrite"));
            fix.extend(drop_unread);
        }
    }
    f.fix = Some(Fix { summary: fix.join(" "), sql, cost: None });
    // On running totals the log it wrote holds every run from before the change: two scans show it, per row
    // (batching writes more of it per run, not per row).
    let now =
        t!("check.log_volume.verify.now", bytes = format::bytes(bytes), rows = format::count(q.rows.unwrap_or(0.0)));
    f.verify = vec![match crate::queries::running_totals(snapshot) {
        Some(source) => t!("check.log_volume.verify.between_scans", log = log, source = source, now = now),
        None => t!("check.log_volume.verify.rescan", log = log),
    }];
    f
}
