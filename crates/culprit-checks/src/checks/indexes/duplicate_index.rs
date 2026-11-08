//! The `duplicate-index` check: Indexes with the same keys as another index, or whose keys are a leading part of a wider one.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn duplicate_index_info() -> CheckInfo {
    CheckInfo {
        id: "duplicate-index".into(),
        title: t!("check.duplicate_index.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![Capability::TableStats],
        description: t!("check.duplicate_index.info.description"),
    }
}

check!(
    DuplicateIndexCheck: duplicate_index_info,
    decide = duplicate_indexes,
    render = duplicate_index_finding,
    case<'s> = DuplicateIndex<'s>,
    evidence = &[Evidence::IndexCatalog],
);

/// The duplicate-index findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn duplicate_index(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&DuplicateIndexCheck, snapshot)
}

/// An index another one makes redundant ([`redundant_indexes`]), and what its finding concludes.
pub struct DuplicateIndex<'s> {
    /// The index to drop.
    index: &'s IndexStat,
    /// The index that serves its queries, and stays.
    keeper: &'s IndexStat,
    /// The same keys as the keeper's, rather than a leading part of them.
    exact: bool,
    size: f64,
    /// Its reads, once the usage counters have counted a week, with the days they counted.
    reads: Option<(f64, Option<f64>)>,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

fn duplicate_indexes(snapshot: &Snapshot) -> Vec<DuplicateIndex<'_>> {
    redundant_indexes(snapshot)
        .into_iter()
        .map(|(index, keeper)| {
            let exact = same_keys(index, keeper);
            let size = index.size_bytes.unwrap_or(0.0);
            let severity = if size >= 512.0 * 1024.0 * 1024.0 { Severity::Medium } else { Severity::Low };
            DuplicateIndex {
                index,
                keeper,
                exact,
                size,
                // Usage counters a few seconds old say nothing about reads ("0" after a restart): the count is shown
                // once they have counted for a week, with how long.
                reads: index
                    .reads
                    .filter(|_| watched_long_enough(snapshot))
                    .map(|r| (r, crate::support::watched_days(snapshot))),
                severity,
                score: base_score(severity) + (size / (1024.0 * 1024.0 * 1024.0)).min(8.0),
                confidence: if exact { Confidence::High } else { Confidence::Medium },
            }
        })
        .collect()
}

fn duplicate_index_finding(snapshot: &Snapshot, d: DuplicateIndex<'_>) -> Finding {
    let (a, b) = (d.index, d.keeper);
    let table_name = format!("{}.{}", a.schema, a.table);
    let mut f = finding(
        "duplicate-index",
        &format!("{}.{}", table_name, a.name),
        Category::Indexes,
        subject(SubjectKind::Index, format!("{}.{}", table_name, a.name)),
    );
    f.title = if d.exact {
        t!("check.duplicate_index.title.same_keys", index = &a.name, keeper = &b.name, table = &a.table)
    } else {
        t!("check.duplicate_index.title.covered", index = &a.name, keeper = &b.name, table = &a.table)
    };
    f.summary = [
        t!(
            "check.duplicate_index.summary.keys",
            index = &a.name,
            keys = a.columns.join(", "),
            keeper = &b.name,
            keeper_keys = b.columns.join(", ")
        ),
        if d.exact {
            t!("check.duplicate_index.summary.same_keys")
        } else {
            t!("check.duplicate_index.summary.covered", index = &a.name, keeper = &b.name)
        },
        t!("check.duplicate_index.summary.upkeep", table = &table_name),
    ]
    .join(" ");
    f.severity = d.severity;
    f.impact.score = d.score;
    f.impact.set_label(t!("check.duplicate_index.impact", size = format::bytes(d.size)));
    f.confidence = d.confidence;
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.size"), d.size, Unit::Bytes));
    if let Some((reads, days)) = d.reads {
        f.metrics.push(Metric::new(t!("common.metric.reads_of", index = &a.name), reads, Unit::Count));
        if let Some(days) = days {
            f.metrics.push(Metric::said(culprit_core::key!("common.metric.counting_for"), days, Unit::Days));
        }
    }
    f.fix = Some(Fix {
        summary: t!("check.duplicate_index.fix", index = &a.name, keeper = &b.name),
        sql: Some(drop_index_for_snapshot(snapshot, &a.schema, &a.table, &a.name)),
        cost: Some(t!("check.duplicate_index.fix_cost", size = format::bytes(d.size))),
    });
    f.verify = vec![
        t!("check.duplicate_index.verify.gone", index = &a.name, keeper = &b.name),
        t!("check.duplicate_index.verify.timing", table = &table_name),
    ];
    if !d.exact {
        f.caveats.push(t!("check.duplicate_index.caveat.narrower", index = &a.name));
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docdb_hash_groups_are_not_equivalent_to_ranges_or_longer_hash_groups() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
        snapshot.server.engine = Engine::Yugabyte;
        let mut a = snapshot.indexes[0].clone();
        a.kind = "lsm".into();
        a.columns = vec!["tenant_id".into()];
        a.docdb = Some(culprit_core::signals::DocdbIndex { hash_columns: vec!["tenant_id".into()], tablets: Some(3) });
        a.is_primary_key = false;
        a.is_unique = false;
        a.is_clustered = false;
        a.is_disabled = false;
        let mut b = a.clone();
        b.name = format!("{}_second", a.name);
        b.docdb.as_mut().unwrap().hash_columns.clear();
        snapshot.indexes = vec![a, b];
        assert!(redundant_indexes(&snapshot).is_empty(), "HASH and ASC serve different access paths");
        snapshot.indexes[1].columns.push("id".into());
        snapshot.indexes[1].docdb.as_mut().unwrap().hash_columns = vec!["tenant_id".into(), "id".into()];
        assert!(redundant_indexes(&snapshot).is_empty(), "the entire composite hash needs equality");
        assert!(!snapshot.indexes[1].can_seek(&["tenant_id".into()]));
        assert!(snapshot.indexes[1].can_seek(&["tenant_id".into(), "id".into()]));
        snapshot.indexes[1].docdb.as_mut().unwrap().hash_columns.pop();
        assert_eq!(redundant_indexes(&snapshot).len(), 1, "the same hash plus ordered suffix can cover a prefix");
        snapshot.indexes[1].docdb = None;
        assert!(redundant_indexes(&snapshot).is_empty(), "missing native metadata cannot prove equivalence");
    }

    #[test]
    fn removing_a_duplicate_checks_the_retained_index_and_query_performance() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mssql-demo.json");
        let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let findings = duplicate_index(&snapshot);
        let covered = findings.iter().find(|f| f.id == "duplicate-index:shop.orders.ix_orders_status").unwrap();
        assert!(covered.verify[0].contains("ix_orders_status_created should still exist"), "{:?}", covered.verify);
        assert!(covered.verify[1].contains("time per run should stay the same or drop"), "{:?}", covered.verify);
        assert!(covered.verify[1].contains("If a query gets slower, check its plan"), "{:?}", covered.verify);
        assert!(findings.iter().all(|f| !f.verify.is_empty()));
    }
}
