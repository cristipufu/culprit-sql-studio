//! The `table-bloat` check: Tables whose pages hold far fewer rows than they could, estimated from row counts and column widths in pg_stats (no extension needed). Every scan still reads the empty space.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn table_bloat_info() -> CheckInfo {
    CheckInfo {
        id: "table-bloat".into(),
        title: t!("check.table_bloat.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Postgres],
        requires: vec![],
        description: t!("check.table_bloat.info.description"),
    }
}

check!(
    TableBloatCheck: table_bloat_info,
    decide = table_bloat,
    render = table_bloat_finding,
    case<'s> = TableBloat<'s>,
    evidence = &[Evidence::Tables(crate::evidence::TableMeasure::Bloat)],
);

pub(super) fn table_heap_bytes(t: &TableStat) -> Option<f64> {
    t.data_bytes.or(t.total_bytes).filter(|b| *b > 0.0)
}

/// Dead rows explain the estimated empty space when their share of the row versions is at least this part of its
/// share: the estimate is good to tens of percent.
pub(crate) const DEAD_EXPLAINS_BLOAT: f64 = 0.9;

/// What table-bloat finds: a table that is mostly empty space, a DEFAULT partition that holds rows, a range-partitioned
/// table about to run out of partitions, or a large B-tree a BRIN index could replace.
pub enum TableBloat<'s> {
    Bloated(Bloated<'s>),
    DefaultPartition(DefaultPartition<'s>),
    Expiring(ExpiringPartition),
    Brin(BrinCandidate<'s>),
}

impl TableBloat<'_> {
    fn severity(&self) -> Severity {
        match self {
            TableBloat::Bloated(b) => b.severity,
            TableBloat::DefaultPartition(d) => d.severity,
            TableBloat::Expiring(e) => e.severity,
            TableBloat::Brin(b) => b.severity,
        }
    }
}

/// A table estimated 40% or more empty space (100 MB or more), or 30% of a gigabyte or more.
pub struct Bloated<'s> {
    table: &'s TableStat,
    bloat: f64,
    size: f64,
    share: f64,
    /// The share of its row versions that are dead, when they explain the empty space: VACUUM is the fix.
    dead: Option<f64>,
    severity: Severity,
}

/// A DEFAULT partition holding 100,000 rows or 100 MB or more.
pub struct DefaultPartition<'s> {
    table: &'s TableStat,
    parent: &'s str,
    rows: f64,
    size: f64,
    severity: Severity,
}

/// A range-partitioned table whose last partition ends within a week, or ended within one, with no DEFAULT or MAXVALUE
/// partition to catch what comes after.
pub struct ExpiringPartition {
    parent: String,
    parts: usize,
    last_part: String,
    bound_literal: String,
    days_remaining: f64,
    severity: Severity,
}

/// A large single-column B-tree on an append-only table's time column, which a BRIN index could replace.
pub struct BrinCandidate<'s> {
    table: &'s TableStat,
    index: &'s IndexStat,
    column: &'s str,
    rows: f64,
    size: f64,
    index_bytes: f64,
    inserts: f64,
    changes: f64,
    severity: Severity,
}

fn table_bloat(snapshot: &Snapshot) -> Vec<TableBloat<'_>> {
    let mut bloated: Vec<Bloated<'_>> = snapshot
        .tables
        .iter()
        .filter_map(|t| {
            let bloat = t.storage.as_ref()?.bloat_bytes?;
            let size = table_heap_bytes(t)?;
            let share = (bloat / size).min(1.0);
            ((bloat >= 100.0 * MB && share >= 0.4) || (bloat >= GB && share >= 0.3)).then_some((t, bloat, size, share))
        })
        .map(|(table, bloat, size, share)| {
            // The estimate counts every byte beyond the live rows, dead rows VACUUM hasn't cleared yet included. When
            // they account for it, VACUUM is the fix, not a rewrite.
            let dead = dead_share(table).filter(|d| *d >= share * DEAD_EXPLAINS_BLOAT);
            let severity = if bloat >= GB && dead.is_none() { Severity::Medium } else { Severity::Low };
            Bloated { table, bloat, size, share, dead, severity }
        })
        .collect();
    bloated.sort_by(|a, b| base_score(b.severity).total_cmp(&base_score(a.severity)));
    bloated.truncate(5);
    let mut cases: Vec<TableBloat<'_>> = bloated.into_iter().map(TableBloat::Bloated).collect();
    cases.extend(default_partitions(snapshot).into_iter().map(TableBloat::DefaultPartition));
    cases.extend(expiring_partitions(snapshot).into_iter().map(TableBloat::Expiring));
    cases.extend(brin_opportunities(snapshot).into_iter().map(TableBloat::Brin));
    cases.sort_by(|a, b| base_score(b.severity()).total_cmp(&base_score(a.severity())));
    cases
}

fn table_bloat_finding(snapshot: &Snapshot, case: TableBloat<'_>) -> Finding {
    match case {
        TableBloat::Bloated(b) => bloated_finding(b),
        TableBloat::DefaultPartition(d) => default_partition_finding(d),
        TableBloat::Expiring(e) => expiring_partition_finding(snapshot, e),
        TableBloat::Brin(b) => brin_finding(b),
    }
}

fn bloated_finding(b: Bloated<'_>) -> Finding {
    let Bloated { table: t, bloat, size, share, dead, severity } = b;
    let name = t.qualified();
    let mut f = finding("table-bloat", &name, Category::Maintenance, subject(SubjectKind::Table, &name));
    f.title =
        t!("check.table_bloat.title", table = &t.name, share = format::percent(share), bloat = format::bytes(bloat));
    f.facts.bloat = Some(culprit_core::FindingBloat { bytes: bloat, share });
    let mut summary = vec![t!(
        "check.table_bloat.summary.estimate",
        table = &t.name,
        needed = format::bytes((size - bloat).max(0.0)),
        size = format::bytes(size)
    )];
    if let Some(d) = dead {
        summary.push(t!("check.table_bloat.summary.dead", share = format::percent(d)));
    }
    f.summary = summary.join(" ");
    f.severity = severity;
    severity_score(&mut f);
    f.impact.label = t!("check.table_bloat.impact", bloat = format::bytes(bloat));
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::new(t!("check.table_bloat.metric.empty"), bloat, Unit::Bytes),
        Metric::new(t!("check.table_bloat.metric.size"), size, Unit::Bytes),
        Metric::new(t!("check.table_bloat.metric.share"), share, Unit::Ratio),
    ];
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    f.fix = Some(if dead.is_some() {
        Fix {
            summary: t!("check.table_bloat.fix.vacuum"),
            sql: Some(format!("VACUUM (ANALYZE) {target};")),
            cost: Some(t!("check.table_bloat.fix.vacuum_cost")),
        }
    } else {
        Fix {
            summary: t!("check.table_bloat.fix.rewrite"),
            sql: Some(format!(
                "-- Exact numbers (needs the pgstattuple extension)\nSELECT * FROM pgstattuple('{}');\n-- Rewrite offline: blocks reads and writes until it finishes\n-- VACUUM (FULL, ANALYZE) {target};",
                target.replace('\'', "''")
            )),
            cost: Some(t!("check.table_bloat.fix.rewrite_cost")),
        }
    });
    f.verify = vec![t!("check.table_bloat.verify")];
    f
}

/// The range-partitioned tables whose last partition ends, or ended, within a week. sequence-capacity reports them on
/// engines table-bloat doesn't run on.
pub(crate) fn expiring_partitions(snapshot: &Snapshot) -> Vec<ExpiringPartition> {
    let Some(now_epoch) = parse_epoch_seconds(&snapshot.taken_at) else {
        return Vec::new();
    };
    let mut by_parent: std::collections::BTreeMap<String, Vec<(String, String)>> = std::collections::BTreeMap::new();
    for t in &snapshot.tables {
        if let Some(parent) = t.partition_of.as_deref() {
            if let Some(bound) = t.storage.as_ref().and_then(|s| s.partition_bounds().into_iter().next()) {
                let parent_qual = if parent.contains('.') || t.schema.is_empty() {
                    parent.to_string()
                } else {
                    format!("{}.{parent}", t.schema)
                };
                let entry = by_parent.entry(parent_qual).or_default();
                if !entry.iter().any(|(name, _)| name == &t.name) {
                    entry.push((t.name.clone(), bound.to_string()));
                }
            }
        } else if let Some(storage) = t.storage.as_ref() {
            let parent = t.qualified();
            for bound in storage.partition_bounds() {
                let (expr, part_name) = bound
                    .rsplit_once("):")
                    .map(|(left, right)| (format!("{left})"), right.to_string()))
                    .unwrap_or_else(|| (bound.to_string(), t.name.clone()));
                let entry = by_parent.entry(parent.clone()).or_default();
                if !entry.iter().any(|(name, _)| name == &part_name) {
                    entry.push((part_name, expr));
                }
            }
        }
    }
    by_parent
        .into_iter()
        .filter_map(|(parent, parts)| {
            let mut parsed = Vec::with_capacity(parts.len());
            for (part_name, bound) in &parts {
                if bound.eq_ignore_ascii_case("default") || bound.to_ascii_uppercase().contains("MAXVALUE") {
                    return None;
                }
                let (literal, epoch) = parse_range_upper_bound(bound)?;
                parsed.push((part_name.as_str(), literal, epoch));
            }
            let (last_part, bound_literal, max_epoch) = parsed.into_iter().max_by(|a, b| a.2.total_cmp(&b.2))?;
            let days_remaining = (max_epoch - now_epoch) / 86_400.0;
            if !(-7.0..=7.0).contains(&days_remaining) {
                return None;
            }
            Some(ExpiringPartition {
                last_part: last_part.to_string(),
                parts: parts.len(),
                parent,
                bound_literal,
                days_remaining,
                severity: if days_remaining <= 2.0 { Severity::High } else { Severity::Medium },
            })
        })
        .collect()
}

/// An expiring partition, worded. Its finding is table-bloat's, whichever check reports it.
pub(crate) fn expiring_partition_finding(_: &Snapshot, e: ExpiringPartition) -> Finding {
    let ExpiringPartition { parent, parts, last_part, bound_literal, days_remaining, severity } = e;
    let parent_short = parent.rsplit('.').next().unwrap_or(&parent);
    let bound_date = bound_literal.get(..10).unwrap_or(&bound_literal);
    let mut f = finding(
        "table-bloat",
        &format!("expiring-partition:{parent}"),
        Category::Maintenance,
        subject(SubjectKind::Table, &parent),
    );
    if days_remaining > 0.0 {
        f.title = t!(
            "check.table_bloat.partition.title.ends",
            parent = parent_short,
            part = &last_part,
            date = bound_date,
            days = format::days(days_remaining)
        );
        f.impact.label = t!("check.table_bloat.partition.impact.ends", days = format::days(days_remaining));
    } else {
        f.title =
            t!("check.table_bloat.partition.title.ended", parent = parent_short, part = &last_part, date = bound_date);
        f.impact.label = t!("check.table_bloat.partition.impact.ended", date = bound_date);
    }
    f.severity = severity;
    severity_score(&mut f);
    f.confidence = Confidence::High;
    f.summary = t!(
        "check.table_bloat.partition.summary",
        parent = &parent,
        n = parts,
        part = &last_part,
        bound = &bound_literal
    );
    f.metrics = vec![
        Metric::new(t!("check.table_bloat.partition.metric.partitions"), parts as f64, Unit::Count),
        Metric::new(t!("check.table_bloat.partition.metric.days"), days_remaining.max(0.0), Unit::Days),
    ];
    f.fix = Some(Fix {
        summary: t!("check.table_bloat.partition.fix", parent = &parent, bound = &bound_literal),
        sql: None,
        cost: Some(t!("check.table_bloat.partition.fix_cost")),
    });
    f.verify = vec![t!("check.table_bloat.partition.verify", parent = &parent)];
    f
}

fn parse_range_upper_bound(bound: &str) -> Option<(String, f64)> {
    let upper = bound.to_ascii_uppercase();
    let after = if upper.starts_with("FOR VALUES FROM") {
        let to_pos = upper.find(" TO (")?;
        bound.get(to_pos + 5..)?.trim_start()
    } else {
        let less_pos = upper.find("VALUES LESS THAN")?;
        let rest = bound.get(less_pos + "VALUES LESS THAN".len()..)?.trim_start();
        rest.strip_prefix('(')?.trim_start()
    };
    let literal = after.strip_prefix('\'')?.split('\'').next()?.trim();
    if literal.is_empty() {
        return None;
    }
    let epoch = if literal.contains(['T', ' ']) {
        parse_epoch_seconds(literal)?
    } else if literal.len() == 10 {
        parse_epoch_seconds(&format!("{literal}T00:00:00Z"))?
    } else {
        return None;
    };
    Some((literal.to_string(), epoch))
}
fn is_time_series_column(snapshot: &Snapshot, schema: &str, table: &str, col_name: &str) -> bool {
    if col_name.is_empty() || !col_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    if snapshot.columns.iter().any(|c| {
        c.schema == schema
            && c.table == table
            && c.name.eq_ignore_ascii_case(col_name)
            && (c.data_type.to_ascii_lowercase().contains("timestamp") || c.data_type.eq_ignore_ascii_case("date"))
    }) {
        return true;
    }
    matches!(
        col_name.to_ascii_lowercase().as_str(),
        "created_at" | "inserted_at" | "occurred_at" | "logged_at" | "recorded_at" | "event_time" | "timestamp"
    )
}

fn default_partitions(snapshot: &Snapshot) -> Vec<DefaultPartition<'_>> {
    snapshot
        .tables
        .iter()
        .filter_map(|t| {
            let parent = t.partition_of.as_deref()?;
            let is_default = t.storage.as_ref().is_some_and(|s| s.is_default_partition());
            if !is_default {
                return None;
            }
            let rows = t.rows.or_else(|| t.storage.as_ref().and_then(|s| s.planner_rows)).unwrap_or(0.0);
            let size = table_heap_bytes(t).unwrap_or(0.0);
            if rows < 100_000.0 && size < 100.0 * MB {
                return None;
            }
            let severity = if rows >= 1_000_000.0 || size >= GB { Severity::Medium } else { Severity::Low };
            Some(DefaultPartition { table: t, parent, rows, size, severity })
        })
        .collect()
}

fn default_partition_finding(d: DefaultPartition<'_>) -> Finding {
    let DefaultPartition { table: t, parent, rows, size, severity } = d;
    let name = t.qualified();
    let mut f = finding(
        "table-bloat",
        &format!("default-partition:{name}"),
        Category::Maintenance,
        subject(SubjectKind::Table, &name),
    );
    let (count, bytes) = (format::count(rows), format::bytes(size));
    f.title = if size > 0.0 {
        t!("check.table_bloat.default.title_size", name = &t.name, rows = &count, size = &bytes)
    } else {
        t!("check.table_bloat.default.title", name = &t.name, rows = &count)
    };
    f.severity = severity;
    severity_score(&mut f);
    f.impact.label = if size > 0.0 {
        t!("check.table_bloat.default.impact_size", size = &bytes)
    } else {
        t!("check.table_bloat.default.impact_rows", rows = &count)
    };
    f.confidence = Confidence::High;
    f.summary = if size > 0.0 {
        t!("check.table_bloat.default.summary_size", name = &t.name, parent = parent, rows = &count, size = &bytes)
    } else {
        t!("check.table_bloat.default.summary", name = &t.name, parent = parent, rows = &count)
    };
    if rows > 0.0 {
        f.metrics.push(Metric::new(t!("check.table_bloat.default.metric.rows"), rows, Unit::Rows));
    }
    if size > 0.0 {
        f.metrics.push(Metric::new(t!("common.metric.size"), size, Unit::Bytes));
    }
    f.fix = Some(Fix {
        summary: t!("check.table_bloat.default.fix", parent = parent, name = &t.name),
        sql: None,
        cost: Some(t!("check.table_bloat.default.fix_cost")),
    });
    f.verify = vec![t!("check.table_bloat.default.verify", name = &t.name)];
    f
}

fn brin_opportunities(snapshot: &Snapshot) -> Vec<BrinCandidate<'_>> {
    if snapshot.server.engine != Engine::Postgres {
        return Vec::new();
    }
    let mut out = Vec::new();
    for t in &snapshot.tables {
        let rows = t.rows.or_else(|| t.storage.as_ref().and_then(|s| s.planner_rows)).unwrap_or(0.0);
        let size = table_heap_bytes(t).unwrap_or(0.0);
        if !rows.is_finite() || !size.is_finite() || rows < 5_000_000.0 || size < 500.0 * MB {
            continue;
        }
        let table_qual = t.qualified();
        // Skip partitioned parent tables (where CREATE INDEX CONCURRENTLY is invalid in PostgreSQL).
        if snapshot.tables.iter().any(|child| child.partition_of.as_deref() == Some(table_qual.as_str()))
            || t.storage.as_ref().is_some_and(|s| !s.partition_bounds().is_empty() && t.partition_of.is_none())
        {
            continue;
        }
        let inserts = t.inserts.unwrap_or(0.0);
        let updates = t.updates.unwrap_or(0.0);
        let deletes = t.deletes.unwrap_or(0.0);
        if !inserts.is_finite()
            || !updates.is_finite()
            || !deletes.is_finite()
            || inserts < 0.0
            || updates < 0.0
            || deletes < 0.0
        {
            continue;
        }
        let total_writes = inserts + updates + deletes;
        if !total_writes.is_finite() || total_writes < 100_000.0 || (updates + deletes) / total_writes > 0.02 {
            continue;
        }
        if t.dead_rows.unwrap_or(0.0) / rows > 0.02 {
            continue;
        }
        for idx in &snapshot.indexes {
            if idx.schema != t.schema || idx.table != t.name {
                continue;
            }
            // Skip unique/PK/disabled/partial/covering/custom-opclass/partitioned or unused indexes (which unused-index handles).
            if idx.is_unique
                || idx.is_primary_key
                || idx.is_disabled
                || idx.filter.is_some()
                || !idx.include.is_empty()
                || idx.opclasses.is_some()
                || idx.partition_of.is_some()
                || idx.reads == Some(0.0)
                || idx.columns.len() != 1
                || (!idx.kind.is_empty() && !idx.kind.eq_ignore_ascii_case("btree"))
            {
                continue;
            }
            let Some(idx_bytes) = idx.size_bytes.filter(|b| b.is_finite() && *b >= 100.0 * MB) else {
                continue;
            };
            let col_name = culprit_core::strip_direction(&idx.columns[0]);
            if !is_time_series_column(snapshot, &t.schema, &t.name, col_name) {
                continue;
            }
            let has_brin = snapshot.indexes.iter().any(|other| {
                other.schema == t.schema
                    && other.table == t.name
                    && other.kind.eq_ignore_ascii_case("brin")
                    && other.key_names().first().is_some_and(|k| k.eq_ignore_ascii_case(col_name))
            });
            if has_brin {
                continue;
            }
            out.push(BrinCandidate {
                table: t,
                index: idx,
                column: col_name,
                rows,
                size,
                index_bytes: idx_bytes,
                inserts,
                changes: updates + deletes,
                severity: if idx_bytes >= GB { Severity::Medium } else { Severity::Low },
            });
        }
    }
    out
}

fn brin_finding(b: BrinCandidate<'_>) -> Finding {
    let BrinCandidate { table: t, index: idx, column: col_name, rows, size, index_bytes, inserts, changes, severity } =
        b;
    let table_qual = t.qualified();
    let mut f = finding(
        "table-bloat",
        &format!("brin-candidate:{table_qual}.{}", idx.name),
        Category::Maintenance,
        subject(SubjectKind::Index, format!("{table_qual}.{}", idx.name)),
    );
    let index_size = format::bytes(index_bytes);
    f.title = t!("check.table_bloat.brin.title", index = &idx.name, table = &t.name, size = &index_size);
    f.severity = severity;
    severity_score(&mut f);
    f.impact.label = t!("check.table_bloat.brin.impact", size = &index_size);
    f.confidence = Confidence::Medium;
    f.summary = t!(
        "check.table_bloat.brin.summary",
        table = &t.name,
        rows = format::count(rows),
        size = format::bytes(size),
        share = format::percent(inserts / (inserts + changes)),
        inserts = format::count(inserts),
        changes = format::count(changes),
        index = &idx.name,
        index_size = &index_size,
        column = col_name
    );
    f.metrics = vec![
        Metric::new(t!("check.table_bloat.brin.metric.index_size"), index_bytes, Unit::Bytes),
        Metric::new(t!("check.table_bloat.brin.metric.rows"), rows, Unit::Rows),
        Metric::new(t!("check.table_bloat.brin.metric.inserts"), inserts, Unit::Rows),
    ];
    let target = qualified(Engine::Postgres, &t.schema, &t.name);
    let brin_name = quote(Engine::Postgres, &format!("ix_{}_{}_brin", t.name, col_name));
    let quoted_col = quote(Engine::Postgres, col_name);
    f.fix = Some(Fix {
        summary: t!("check.table_bloat.brin.fix", column = col_name, index = &idx.name),
        sql: Some(format!("CREATE INDEX CONCURRENTLY {brin_name} ON {target} USING brin ({quoted_col});")),
        cost: Some(t!("check.table_bloat.brin.fix_cost")),
    });
    f.verify = vec![t!("check.table_bloat.brin.verify", brin = brin_name.trim_matches('"'), index = &idx.name)];
    f
}
