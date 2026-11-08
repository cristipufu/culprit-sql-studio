//! SQLite's own findings, from what the file records about itself (`crates/culprit-sqlite`): whether the planner has
//! statistics, and how much of the file is free pages. What only the application knows isn't judged: whether several
//! processes share the file decides if the rollback journal hurts, so the journal mode is shown as a setting, never as
//! a finding.

use super::*;

/// Free pages worth a finding: this many bytes, and this share of the file.
const FREE_BYTES_AT_LEAST: f64 = 16.0 * 1024.0 * 1024.0;
const FREE_SHARE_AT_LEAST: f64 = 0.25;

/// What the file establishes, one finding each.
pub enum Case<'s> {
    /// No planner statistics while there are this many secondary indexes to choose between; `rows_known`: every
    /// table's size is.
    NoStatistics { secondary: usize, rows_known: bool },
    /// Pages deleted rows left behind: their bytes, their share of the file, and whether it vacuums incrementally.
    FreePages { free_bytes: f64, share: f64, incremental: bool },
    /// This many foreign keys declared while PRAGMA foreign_keys is off.
    ForeignKeysOff { count: usize },
    /// A table whose indexes lack statistics while sqlite_stat1 exists.
    IndexStatistics { table: &'s culprit_core::TableStat, missing: Vec<&'s culprit_core::IndexStat> },
}

/// The file's findings, under the `config` check.
pub(super) fn decide(snapshot: &Snapshot) -> Vec<Case<'_>> {
    let mut out: Vec<Case<'_>> =
        [statistics(snapshot), free_space(snapshot), foreign_keys_off(snapshot)].into_iter().flatten().collect();
    if snapshot.server.has(Capability::Statistics) {
        for t in snapshot.tables.iter().filter(|t| !t.is_virtual()) {
            let missing: Vec<_> = snapshot
                .indexes_on(&t.schema, &t.name)
                .filter(|i| !i.is_primary_key && i.has_statistics == Some(false))
                .collect();
            if missing.is_empty() || t.rows.is_some_and(|r| r < 10_000.0) {
                continue;
            }
            out.push(Case::IndexStatistics { table: t, missing });
        }
    }
    out
}

pub(super) fn render(snapshot: &Snapshot, case: Case<'_>) -> Finding {
    match case {
        Case::NoStatistics { secondary, rows_known } => statistics_finding(snapshot, secondary, rows_known),
        Case::FreePages { free_bytes, share, incremental } => {
            free_space_finding(snapshot, free_bytes, share, incremental)
        }
        Case::ForeignKeysOff { count } => foreign_keys_off_finding(snapshot, count),
        Case::IndexStatistics { table: t, missing } => {
            let mut f = finding(
                "config",
                &format!("sqlite-statistics:{}", t.qualified()),
                Category::Maintenance,
                subject(SubjectKind::Table, t.qualified()),
            );
            let count = missing.len().to_string();
            f.title =
                t!("check.config.sqlite_index_statistics.title", table = &t.name, n = missing.len(), count = &count);
            f.summary = t!("check.config.sqlite_index_statistics.summary");
            f.severity = Severity::Low;
            f.confidence = Confidence::High;
            f.impact.score = base_score(Severity::Low);
            f.impact.label = t!("check.config.sqlite_index_statistics.impact", n = missing.len(), count = &count);
            f.metrics.push(Metric::text(
                t!("check.config.sqlite_index_statistics.metric.indexes"),
                missing.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "),
            ));
            f.fix = Some(Fix {
                summary: t!("check.config.sqlite_index_statistics.fix"),
                sql: Some(format!(
                    "ANALYZE {};",
                    snapshot.server.engine.spec().dialect().qualified(&[&t.schema, &t.name])
                )),
                cost: Some(t!("check.config.sqlite_index_statistics.fix_cost")),
            });
            f.verify = vec![t!("check.config.sqlite_index_statistics.verify")];
            f
        }
    }
}

/// No planner statistics while the planner has indexes to choose between.
fn statistics(s: &Snapshot) -> Option<Case<'_>> {
    if s.server.has(Capability::Statistics) {
        return None;
    }
    let secondary = s.indexes.iter().filter(|i| !i.is_primary_key).count();
    if secondary == 0 {
        return None;
    }
    // On tables this small (rows counted), a wrong guess costs nothing worth a finding.
    let tables: Vec<_> = s.tables.iter().filter(|t| !t.is_virtual()).collect();
    let rows_known = !tables.is_empty() && tables.iter().all(|t| t.rows.is_some());
    if rows_known && tables.iter().filter_map(|t| t.rows).fold(0.0, f64::max) < 10_000.0 {
        return None;
    }
    Some(Case::NoStatistics { secondary, rows_known })
}

fn statistics_finding(s: &Snapshot, secondary: usize, rows_known: bool) -> Finding {
    let mut f = finding(
        "config",
        "sqlite-statistics",
        Category::Maintenance,
        subject(SubjectKind::Database, &s.server.database),
    );
    f.title = t!("check.config.sqlite_statistics.title");
    let mut summary = vec![t!("check.config.sqlite_statistics.summary", n = secondary, count = secondary.to_string())];
    if !rows_known {
        summary.push(t!("check.config.sqlite_statistics.sizes_unknown"));
    }
    f.summary = summary.join(" ");
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.label = t!("check.config.sqlite_statistics.impact");
    f.confidence = Confidence::High;
    f.metrics.push(Metric::new(t!("check.config.sqlite_statistics.metric.secondary"), secondary as f64, Unit::Count));
    f.fix = Some(Fix {
        summary: t!("check.config.sqlite_statistics.fix"),
        sql: Some("ANALYZE;".into()),
        cost: Some(t!("check.config.sqlite_statistics.fix_cost")),
    });
    f.verify = vec![t!("check.config.sqlite_statistics.verify")];
    f
}

/// Pages that deleted rows left behind, when they are many and a large part of the file.
fn free_space(s: &Snapshot) -> Option<Case<'_>> {
    let number =
        |name: &str| s.settings.iter().find(|x| x.name == name).and_then(|x| x.value.trim().parse::<f64>().ok());
    let (free, pages, page_size) = (number("freelist_count")?, number("page_count")?, number("page_size")?);
    let free_bytes = free * page_size;
    let share = free / pages.max(1.0);
    if free_bytes < FREE_BYTES_AT_LEAST || share < FREE_SHARE_AT_LEAST {
        return None;
    }
    // auto_vacuum: 0 none, 1 full, 2 incremental.
    Some(Case::FreePages { free_bytes, share, incremental: number("auto_vacuum") == Some(2.0) })
}

fn free_space_finding(s: &Snapshot, free_bytes: f64, share: f64, incremental: bool) -> Finding {
    let mut f = finding(
        "config",
        "sqlite-free-pages",
        Category::Maintenance,
        subject(SubjectKind::Database, &s.server.database),
    );
    let size = format::bytes(free_bytes);
    f.title = t!("check.config.sqlite_free_pages.title", size = &size, share = format::percent(share));
    f.summary = t!("check.config.sqlite_free_pages.summary");
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.label = t!("check.config.sqlite_free_pages.impact", size = &size);
    f.confidence = Confidence::High;
    f.metrics.push(Metric::new(t!("check.config.sqlite_free_pages.metric.free_pages"), free_bytes, Unit::Bytes));
    f.metrics.push(Metric::new(t!("check.config.sqlite_free_pages.metric.share"), share, Unit::Ratio));
    f.fix = Some(if incremental {
        Fix {
            summary: t!("check.config.sqlite_free_pages.fix.incremental"),
            sql: Some("PRAGMA incremental_vacuum;".into()),
            cost: None,
        }
    } else {
        Fix {
            summary: t!("check.config.sqlite_free_pages.fix.vacuum"),
            sql: Some("PRAGMA auto_vacuum = INCREMENTAL;\nVACUUM;".into()),
            cost: Some(t!("check.config.sqlite_free_pages.fix_cost.vacuum")),
        }
    });
    f.verify = vec![t!("check.config.sqlite_free_pages.verify")];
    f
}

/// Foreign key constraints exist in the schema, while the connection's `foreign_keys` pragma was recorded as OFF.
fn foreign_keys_off(s: &Snapshot) -> Option<Case<'_>> {
    if s.foreign_keys.is_empty() {
        return None;
    }
    let setting = s.setting("foreign_keys")?;
    if is_on(&setting.value) {
        return None;
    }
    Some(Case::ForeignKeysOff { count: s.foreign_keys.len() })
}

fn foreign_keys_off_finding(s: &Snapshot, count: usize) -> Finding {
    let mut f = finding(
        "config",
        "sqlite-foreign-keys",
        Category::Maintenance,
        subject(SubjectKind::Database, &s.server.database),
    );
    let said = count.to_string();
    f.title = t!("check.config.sqlite_foreign_keys.title");
    f.summary = t!("check.config.sqlite_foreign_keys.summary", n = count, count = &said);
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.label = t!("check.config.sqlite_foreign_keys.impact", n = count, count = &said);
    f.confidence = Confidence::High;
    f.metrics.push(Metric::new(t!("check.config.sqlite_foreign_keys.metric.foreign_keys"), count as f64, Unit::Count));
    f.fix = Some(Fix {
        summary: t!("check.config.sqlite_foreign_keys.fix"),
        sql: Some("PRAGMA foreign_key_check;\nPRAGMA foreign_keys = ON;".into()),
        cost: None,
    });
    f.verify = vec![t!("check.config.sqlite_foreign_keys.verify")];
    f
}

/// The findings this file makes of `snapshot`.
#[cfg(test)]
pub(super) fn check(snapshot: &Snapshot) -> Vec<Finding> {
    decide(snapshot).into_iter().map(|case| render(snapshot, case)).collect()
}
