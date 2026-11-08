//! The `overlapping-index` check: Two indexes that lead with the same column where one is barely read and the other holds all its key columns: queries that use the first can seek the second and filter the rest there.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn overlapping_index_info() -> CheckInfo {
    CheckInfo {
        id: "overlapping-index".into(),
        title: t!("check.overlapping_index.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![Capability::IndexUsage],
        description: t!("check.overlapping_index.info.description"),
    }
}

check!(
    OverlappingIndexCheck: overlapping_index_info,
    decide = overlapping_indexes,
    render = overlapping_index_finding,
    case<'s> = OverlappingIndex<'s>,
    evidence = &[Evidence::IndexCatalog],
);

/// Indexes that compete for the same leading column, as (rarely read, well used). The well-used one must hold every key
/// column of the other as a key of its own: sharing only the first column says nothing when that column isn't
/// selective (a tenant id in a multi-tenant schema leads almost every index, and one tenant can be most rows), and an
/// included column is no part of the index's search (PostgreSQL may check it only on the table's row). It must hold every
/// row the other does, so no filter, and it must not be the clustered index (InnoDB's PRIMARY too): seeking that reads
/// whole rows, which is what the narrow index spares its queries.
pub(crate) fn overlapping_pairs(snapshot: &Snapshot) -> Vec<(&IndexStat, &IndexStat)> {
    // "Barely read" is a claim about a week of counters, the same as "unused": under it, reads say nothing yet.
    if !crate::indexes::watched_long_enough(snapshot) {
        return Vec::new();
    }
    // By table and name: MySQL and SQL Server index names are unique only within a table.
    let redundant: Vec<(&str, &str, &str)> = redundant_indexes(snapshot)
        .iter()
        .map(|(a, _)| (a.schema.as_str(), a.table.as_str(), a.name.as_str()))
        .collect();
    let mut out: Vec<(&IndexStat, &IndexStat)> = Vec::new();
    for a in &snapshot.indexes {
        let plain = |i: &IndexStat| {
            !i.is_primary_key
                && !i.is_unique
                && !i.is_clustered
                && !i.is_disabled
                && i.filter.is_none()
                && !crate::indexes::is_partition_copy(i)
        };
        // Rarely read says nothing about an index that may have been created this week.
        if !plain(a)
            || redundant.contains(&(a.schema.as_str(), a.table.as_str(), a.name.as_str()))
            || is_unused(snapshot, a)
            || too_new(snapshot, a)
            || a.columns.is_empty()
        {
            continue;
        }
        let Some(reads) = a.reads else { continue };
        let lead = a.key_names()[0].to_ascii_lowercase();
        let better = snapshot
            .indexes_on(&a.schema, &a.table)
            .filter(|b| {
                b.name != a.name
                    && !b.is_disabled
                    && !b.is_clustered
                    && b.same_filter(a)
                    && b.kind == a.kind
                    && a.same_sharding(b)
                    && !same_keys(a, b)
            })
            .filter(|b| b.key_names().first().is_some_and(|k| k.eq_ignore_ascii_case(&lead)))
            .filter(|b| {
                let held = b.key_names();
                a.key_names().iter().all(|k| held.iter().any(|h| h.eq_ignore_ascii_case(k)))
            })
            .filter(|b| b.reads.is_some_and(|r| r >= 1000.0 && reads * 100.0 <= r))
            .max_by(|x, y| x.reads.unwrap_or(0.0).total_cmp(&y.reads.unwrap_or(0.0)));
        if let Some(b) = better
            && !out.iter().any(|(x, _)| x.name == a.name)
        {
            out.push((a, b));
        }
    }
    out
}

/// An index barely read next to a better-used one that leads with the same column ([`overlapping_pairs`]), and what its
/// finding concludes.
pub struct OverlappingIndex<'s> {
    /// The index barely read.
    index: &'s IndexStat,
    /// The index that leads with the same column and holds all its keys.
    wider: &'s IndexStat,
    /// Their first key.
    lead: &'s str,
    reads: f64,
    wider_reads: f64,
    size: f64,
    /// The foreign key the barely-read index also supports.
    foreign_key: Option<&'s culprit_core::ForeignKey>,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

fn overlapping_indexes(snapshot: &Snapshot) -> Vec<OverlappingIndex<'_>> {
    overlapping_pairs(snapshot)
        .into_iter()
        .map(|(index, wider)| {
            let size = index.size_bytes.unwrap_or(0.0);
            let severity = Severity::Low;
            OverlappingIndex {
                index,
                wider,
                lead: index.key_names()[0],
                reads: index.reads.unwrap_or(0.0),
                wider_reads: wider.reads.unwrap_or(0.0),
                size,
                foreign_key: backs_foreign_key(snapshot, index),
                severity,
                score: base_score(severity) + (size / (1024.0 * 1024.0 * 1024.0)).min(6.0),
                confidence: Confidence::Low,
            }
        })
        .collect()
}

fn overlapping_index_finding(snapshot: &Snapshot, o: OverlappingIndex<'_>) -> Finding {
    let (a, b) = (o.index, o.wider);
    let table_name = format!("{}.{}", a.schema, a.table);
    let mut f = finding(
        "overlapping-index",
        &format!("{}.{}", table_name, a.name),
        Category::Indexes,
        subject(SubjectKind::Index, format!("{}.{}", table_name, a.name)),
    );
    f.title = t!("check.overlapping_index.title", index = &a.name, wider = &b.name, lead = o.lead);
    f.summary = [
        t!(
            "check.overlapping_index.summary.keys",
            index = &a.name,
            keys = a.columns.join(", "),
            wider = &b.name,
            wider_keys = b.columns.join(", "),
            lead = o.lead,
            table = &table_name
        ),
        t!(
            "check.overlapping_index.summary.reads",
            index = &a.name,
            n = o.reads,
            reads = format::count(o.reads),
            since = since_phrase(snapshot),
            wider = &b.name,
            wider_n = o.wider_reads,
            wider_reads = format::count(o.wider_reads)
        ),
        t!("check.overlapping_index.summary.seek", lead = o.lead, table = &table_name),
    ]
    .join(" ");
    f.severity = o.severity;
    f.impact.score = o.score;
    f.impact.label = t!("check.overlapping_index.impact", size = format::bytes(o.size));
    f.confidence = o.confidence;
    f.metrics.push(Metric::new(t!("common.metric.reads_of", index = &a.name), o.reads, Unit::Count));
    f.metrics.push(Metric::new(t!("common.metric.reads_of", index = &b.name), o.wider_reads, Unit::Count));
    f.metrics.push(Metric::new(t!("common.metric.size"), o.size, Unit::Bytes));
    f.fix = Some(Fix {
        summary: t!("check.overlapping_index.fix", index = &a.name, wider = &b.name),
        sql: Some(hide_index_sql(snapshot, a)),
        cost: Some(t!("check.overlapping_index.fix_cost", size = format::bytes(o.size))),
    });
    f.verify = vec![t!(
        "check.overlapping_index.verify",
        index = &a.name,
        table = &a.table,
        wider = &b.name,
        reads = format::count(o.reads)
    )];
    f.caveats.push(t!("check.overlapping_index.caveat.covers", index = &a.name, wider = &b.name));
    if let Some(fk) = o.foreign_key {
        f.caveats.push(t!("check.overlapping_index.caveat.foreign_key", index = &a.name, foreign_key = &fk.name));
    }
    f
}
