//! The `unused-index` check: Indexes that are maintained on every write but no query has read since usage counters were reset. One that may be less than a week old (a scan saw it appear, or its table changed before Culprit's first scan) isn't called unused yet.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn unused_index_info() -> CheckInfo {
    CheckInfo {
        id: "unused-index".into(),
        title: t!("check.unused_index.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![Capability::IndexUsage],
        description: t!("check.unused_index.info.description"),
    }
}

check!(
    UnusedIndexCheck: unused_index_info,
    decide = unused_index_cases,
    render = unused_index_finding,
    case<'s> = UnusedIndex<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::IndexHistory],
);

/// The unused-index findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn unused_index(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&UnusedIndexCheck, snapshot)
}

/// What the unused-index check concludes about one subject.
pub enum UnusedIndex<'s> {
    /// A table's indexes that no query read.
    Unread(UnreadIndexes<'s>),
    /// A PostgreSQL index an unfinished build left invalid.
    Invalid(InvalidIndex<'s>),
}

/// The indexes of one table that no query read ([`unused_indexes`]), and what they cost.
pub struct UnreadIndexes<'s> {
    schema: &'s str,
    table: &'s str,
    indexes: Vec<&'s IndexStat>,
    size: f64,
    /// The writes that maintained them since the counters started.
    writes: Option<f64>,
    /// SQL Server counts the writes that maintained each index (`user_updates`), not the table's: the number is the
    /// index's own. MySQL's per-index count is the table's writes, and PostgreSQL keeps only the table's.
    index_counted: bool,
    /// The youngest index, where it may be younger than the counters: its age in days, and the latest it may have been
    /// created. It has gone unread only since then.
    youngest: Option<(f64, &'s str)>,
    /// The days they have surely gone unread.
    days: Option<f64>,
    /// Those that lead with a foreign key's columns, with the key.
    foreign_keys: Vec<(&'s IndexStat, &'s culprit_core::ForeignKey)>,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

/// A PostgreSQL index left invalid by an unfinished concurrent build, that nothing else accounts for.
pub struct InvalidIndex<'s> {
    index: &'s IndexStat,
    size: f64,
    /// The table's writes since the counters started.
    writes: Option<f64>,
    /// Writes still update it.
    maintained: bool,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

fn unused_index_cases(snapshot: &Snapshot) -> Vec<UnusedIndex<'_>> {
    let engine = snapshot.server.engine;
    // What the counters have surely watched: on performance_schema counters that may have been reset, the digests' days.
    let days = crate::support::watched_days(snapshot);
    let mut by_table: Vec<(&str, &str, Vec<&IndexStat>)> = Vec::new();
    for i in unused_indexes(snapshot) {
        match by_table.iter_mut().find(|(s, t, _)| *s == i.schema && *t == i.table) {
            Some((_, _, list)) => list.push(i),
            None => by_table.push((&i.schema, &i.table, vec![i])),
        }
    }
    by_table
        .into_iter()
        .map(|(schema, table, indexes)| {
            let size: f64 = indexes.iter().filter_map(|i| i.size_bytes).sum();
            let writes = table_writes(snapshot, schema, table, &indexes);
            let index_counted = engine == Engine::Mssql && indexes.iter().any(|i| i.writes.is_some());
            // An index that may be younger than the counters has gone unread only since it may have been created.
            let youngest = indexes
                .iter()
                .filter_map(|i| Some((index_age_days(snapshot, i)?, i.created_at_latest.as_deref()?)))
                .filter(|(age, _)| days.is_none_or(|d| *age < d))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            let days = youngest.map(|(age, _)| age).or(days);
            // What the index costs: its size, or the upkeep of a busy table, as a rate (a total since a restart months
            // ago says nothing about today).
            let writes_a_day = writes.and_then(|w| counter_rate_per_day(snapshot, w));
            let severity = if size >= 1024.0 * 1024.0 * 1024.0 || writes_a_day.is_some_and(|w| w >= UPKEEP_A_DAY) {
                Severity::Medium
            } else {
                Severity::Low
            };
            let foreign_keys =
                indexes.iter().filter_map(|i| backs_foreign_key(snapshot, i).map(|fk| (*i, fk))).collect();
            UnusedIndex::Unread(UnreadIndexes {
                schema,
                table,
                size,
                writes,
                index_counted,
                youngest,
                days,
                foreign_keys,
                severity,
                score: base_score(severity) + (size / (1024.0 * 1024.0 * 1024.0)).min(10.0),
                confidence: match days {
                    Some(d) if d >= 30.0 => Confidence::High,
                    Some(d) if d >= 7.0 => Confidence::Medium,
                    _ => Confidence::Low,
                },
                indexes,
            })
        })
        .chain(invalid_postgres_indexes(snapshot, &covered(snapshot)).into_iter().map(UnusedIndex::Invalid))
        .collect()
}

fn unused_index_finding(snapshot: &Snapshot, case: UnusedIndex<'_>) -> Finding {
    match case {
        UnusedIndex::Unread(u) => unread_finding(snapshot, u),
        UnusedIndex::Invalid(i) => invalid_finding(snapshot, i),
    }
}

fn unread_finding(snapshot: &Snapshot, u: UnreadIndexes<'_>) -> Finding {
    let (schema, table) = (u.schema, u.table);
    let table_name = format!("{schema}.{table}");
    let n = u.indexes.len();
    let since = match u.youngest {
        Some((age, created)) => t!(
            "check.unused_index.since_created",
            days = age.round().to_string(),
            date = format::friendly_date(created, &snapshot.taken_at),
            n = n
        ),
        None => since_phrase(snapshot),
    };
    let mut f = finding("unused-index", &table_name, Category::Indexes, subject(SubjectKind::Table, &table_name));
    f.title = match u.indexes.as_slice() {
        [one] => t!("check.unused_index.title.one", index = &one.name, table = table),
        many => t!("check.unused_index.title.many", count = many.len().to_string(), table = table),
    };
    let indexes = format::list(&u.indexes.iter().map(|i| i.name.clone()).collect::<Vec<_>>());
    let size = format::bytes(u.size);
    f.summary = match u.writes {
        // Nothing written either: the cost is the space, not the upkeep.
        Some(0.0) => [
            if u.index_counted {
                t!("check.unused_index.summary.unwritten_index", indexes = &indexes, since = &since, n = n)
            } else {
                t!(
                    "check.unused_index.summary.unwritten_table",
                    indexes = &indexes,
                    since = &since,
                    table = &table_name
                )
            },
            t!("check.unused_index.summary.space", size = &size, n = n),
        ],
        writes => [
            match writes {
                Some(w) if u.index_counted => t!(
                    "check.unused_index.summary.unread_index_written",
                    indexes = &indexes,
                    since = &since,
                    writes = format::count(w),
                    n = n
                ),
                Some(w) => t!(
                    "check.unused_index.summary.unread_table_written",
                    indexes = &indexes,
                    since = &since,
                    table = &table_name,
                    writes = format::count(w)
                ),
                None => t!("check.unused_index.summary.unread", indexes = &indexes, since = &since),
            },
            t!("check.unused_index.summary.upkeep", size = &size, n = n),
        ],
    }
    .join(" ");
    f.severity = u.severity;
    f.impact.score = u.score;
    f.impact.set_label(t!("check.unused_index.impact", size = &size));
    f.confidence = u.confidence;
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.size"), u.size, Unit::Bytes));
    if let Some(w) = u.writes {
        let label = if u.index_counted {
            t!("check.unused_index.metric.index_writes")
        } else {
            t!("check.unused_index.metric.table_writes")
        };
        f.metrics.push(Metric::new(label, w, Unit::Count));
    }
    if let Some(d) = u.days {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.counting_for"), d, Unit::Days));
    }
    let sql = u.indexes.iter().map(|i| crate::index_health::hide_index_sql(snapshot, i)).collect::<Vec<_>>().join("\n");
    let kind = snapshot.table(schema, table).and_then(|t| t.kind.as_deref());
    f.fix = Some(Fix {
        summary: snapshot.server.engine.spec().unused_index_advice(&snapshot.server, kind),
        sql: Some(sql),
        cost: Some(t!("check.unused_index.fix_cost", size = &size, table = &table_name)),
    });
    f.verify = vec![t!("check.unused_index.verify.unread")];
    f.caveats.push(t!("check.unused_index.caveat.restarts"));
    f.caveats.push(t!("check.unused_index.caveat.replicas"));
    if !u.foreign_keys.is_empty() {
        let backers: Vec<String> = u
            .foreign_keys
            .iter()
            .map(|(i, fk)| t!("check.unused_index.caveat.backer", index = &i.name, foreign_key = &fk.name))
            .collect();
        f.caveats.push(t!(
            "check.unused_index.caveat.foreign_keys",
            backers = format::list(&backers),
            n = backers.len()
        ));
    }
    if let Some(defs) = definitions(&u.indexes) {
        f.caveats.push(t!("check.unused_index.caveat.definitions", definitions = defs));
    }
    f
}

/// Orphaned PostgreSQL `INVALID` indexes left behind by an unfinished `CREATE INDEX CONCURRENTLY` or
/// `REINDEX CONCURRENTLY` that neither duplicate an active index (`redundant_indexes`) nor serve a scan that
/// recommends rebuilding them (`disabled_leading_index`).
fn invalid_postgres_indexes<'s>(snapshot: &'s Snapshot, redundant: &[(&str, &str, &str)]) -> Vec<InvalidIndex<'s>> {
    if snapshot.server.engine != Engine::Postgres || !watched_long_enough(snapshot) {
        return Vec::new();
    }
    let scans = scan_groups(snapshot);
    let rebuilt_by_scan = |i: &IndexStat| {
        scans.iter().any(|g| {
            disabled_leading_index(snapshot, g).is_some_and(|d| {
                d.schema.eq_ignore_ascii_case(&i.schema)
                    && d.table.eq_ignore_ascii_case(&i.table)
                    && d.name.eq_ignore_ascii_case(&i.name)
            })
        })
    };
    snapshot
        .indexes
        .iter()
        .filter(|i| {
            i.is_disabled
                && !i.is_primary_key
                && !is_partition_copy(i)
                && !redundant.contains(&(i.schema.as_str(), i.table.as_str(), i.name.as_str()))
                && !rebuilt_by_scan(i)
        })
        .map(|i| {
            let size = i.size_bytes.unwrap_or(0.0);
            let writes = table_writes(snapshot, &i.schema, &i.table, &[i]);
            let writes_a_day = writes.and_then(|w| counter_rate_per_day(snapshot, w));
            let maintained = i.maintained != Some(false);
            let severity = if maintained
                && (size >= 1024.0 * 1024.0 * 1024.0 || writes_a_day.is_some_and(|w| w >= UPKEEP_A_DAY))
            {
                Severity::Medium
            } else {
                Severity::Low
            };
            InvalidIndex {
                index: i,
                size,
                writes,
                maintained,
                severity,
                score: base_score(severity) + (size / (1024.0 * 1024.0 * 1024.0)).min(10.0),
                confidence: Confidence::High,
            }
        })
        .collect()
}

fn invalid_finding(snapshot: &Snapshot, v: InvalidIndex<'_>) -> Finding {
    let i = v.index;
    let table_name = format!("{}.{}", i.schema, i.table);
    let index_name = format!("{table_name}.{}", i.name);
    let size = format::bytes(v.size);
    let mut f = finding(
        "unused-index",
        &format!("invalid:{index_name}"),
        Category::Indexes,
        subject(SubjectKind::Index, &index_name),
    );
    f.title = t!("check.unused_index.title.invalid", index = &i.name, table = &i.table);
    f.summary = [
        t!("check.unused_index.summary.invalid", index = &i.name),
        t!(
            "check.unused_index.summary.invalid_never_read",
            maintained = if v.maintained { "yes" } else { "no" },
            table = &table_name,
            sized = if v.size > 0.0 { "yes" } else { "no" },
            size = &size
        ),
    ]
    .join(" ");
    f.severity = v.severity;
    f.impact.score = v.score;
    f.impact.set_label(if v.size > 0.0 {
        t!("check.unused_index.impact.invalid", size = &size)
    } else {
        t!("check.unused_index.impact.invalid_unsized")
    });
    f.confidence = v.confidence;
    if v.size > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.size"), v.size, Unit::Bytes));
    }
    if let Some(w) = v.writes {
        f.metrics.push(Metric::said(culprit_core::key!("check.unused_index.metric.table_writes"), w, Unit::Count));
    }
    f.fix = Some(Fix {
        summary: t!("check.unused_index.fix.invalid", table = &table_name),
        sql: Some(drop_index_for_snapshot(snapshot, &i.schema, &i.table, &i.name)),
        cost: (v.size > 0.0).then(|| t!("check.unused_index.fix_cost", size = &size, table = &table_name)),
    });
    f.verify = vec![t!("check.unused_index.verify.invalid", index = &i.name, table = &table_name)];
    if let Some(defs) = definitions(&[i]) {
        f.caveats.push(t!("check.unused_index.caveat.definition", definition = defs));
    }
    f
}

/// The indexes this check calls unused ([`is_unused`]), less those a wider index covers (the duplicate-index check says
/// those) and, on MySQL, those a foreign key relies on (InnoDB refuses to drop them). The schema view's notes say
/// "unused" of exactly these.
pub(crate) fn unused_indexes(snapshot: &Snapshot) -> Vec<&IndexStat> {
    let redundant = covered(snapshot);
    let mysql = snapshot.server.engine == Engine::Mysql;
    snapshot
        .indexes
        .iter()
        .filter(|i| {
            is_unused(snapshot, i) && !redundant.contains(&(i.schema.as_str(), i.table.as_str(), i.name.as_str()))
        })
        .filter(|i| !(mysql && backs_foreign_key(snapshot, i).is_some()))
        .collect()
}

/// The indexes a wider one covers, by table and name: MySQL and SQL Server index names are unique only within a table.
fn covered(snapshot: &Snapshot) -> Vec<(&str, &str, &str)> {
    redundant_indexes(snapshot).iter().map(|(a, _)| (a.schema.as_str(), a.table.as_str(), a.name.as_str())).collect()
}

/// Writes a day to a table that make keeping an unread index up to date a Medium problem.
pub(super) const UPKEEP_A_DAY: f64 = 100_000.0;

/// The foreign key whose columns this index leads with, if any: dropping it would leave that foreign key unindexed.
pub(crate) fn backs_foreign_key<'a>(snapshot: &'a Snapshot, index: &IndexStat) -> Option<&'a culprit_core::ForeignKey> {
    let keys: Vec<String> = index.key_names().iter().map(|k| k.to_ascii_lowercase()).collect();
    snapshot.foreign_keys.iter().find(|fk| {
        fk.schema.eq_ignore_ascii_case(&index.schema)
            && fk.table.eq_ignore_ascii_case(&index.table)
            && !fk.columns.is_empty()
            && index.can_seek(&fk.columns)
            && keys.len() >= fk.columns.len()
            && lower_set(&keys[..fk.columns.len()]) == lower_set(&fk.columns)
    })
}

pub(super) fn table_writes(snapshot: &Snapshot, schema: &str, table: &str, list: &[&IndexStat]) -> Option<f64> {
    if let Some(w) = list.iter().filter_map(|i| i.writes).reduce(f64::max) {
        return Some(w);
    }
    let t = snapshot.table(schema, table)?;
    Some(t.inserts? + t.updates.unwrap_or(0.0) + t.deletes.unwrap_or(0.0))
}
