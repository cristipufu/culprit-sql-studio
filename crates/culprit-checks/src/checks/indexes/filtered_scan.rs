//! The `filtered-scan` check: Heavy queries whose plan reads a large table in full and keeps only the rows matching a filter that no index supports.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::t;

pub fn filtered_scan_info() -> CheckInfo {
    CheckInfo {
        id: "filtered-scan".into(),
        title: t!("check.filtered_scan.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql],
        requires: vec![Capability::QueryPlans],
        description: t!("check.filtered_scan.info.description"),
    }
}

check!(
    FilteredScanCheck: filtered_scan_info,
    decide = filtered_scans,
    render = filtered_scan_finding,
    case<'s> = FilteredScan<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::Query(crate::evidence::QueryMeasure::Plans)],
);

pub(crate) struct ScanGroup<'a> {
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) keys: Vec<String>,
    pub(crate) predicate: String,
    pub(crate) rows: f64,
    pub(crate) queries: Vec<(&'a QueryStat, u32)>,
}

impl ScanGroup<'_> {
    /// Keep compilation-time cardinality apart from the catalog collected for this scan. This value is a basis for
    /// selectivity, not a measurement of every execution's reads (TOP, parameters and repeated scans can differ).
    pub(crate) fn row_basis(&self, snapshot: &Snapshot) -> String {
        let rows = format::count(self.rows);
        match snapshot.table(&self.schema, &self.table).and_then(|t| t.rows) {
            Some(table_rows) => {
                t!("check.filtered_scan.row_basis", rows = rows, table_rows = format::count(table_rows))
            }
            None => t!("check.filtered_scan.row_basis_unknown", rows = rows),
        }
    }
}

/// The filtered-scan findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn filtered_scan(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&FilteredScanCheck, snapshot)
}

/// Full scans of large tables that keep only rows matching a filter no index leads with, grouped by table and keys.
pub(crate) fn scan_groups(snapshot: &Snapshot) -> Vec<ScanGroup<'_>> {
    let mut groups: Vec<ScanGroup> = Vec::new();
    for q in app_queries(snapshot) {
        let Some(plan) = &q.plan else { continue };
        if query_impact(snapshot, &[q]).is_none_or(|i| i.share < 0.005) {
            continue;
        }
        for node in plan.nodes() {
            if !matches!(node.kind, culprit_core::NodeKind::Scan | culprit_core::NodeKind::Seek) {
                continue;
            }
            if node.kind == culprit_core::NodeKind::Seek && node.seek_predicate.is_none() {
                continue;
            }
            let filter_parent =
                plan.nodes().into_iter().find(|p| p.op == "Filter" && p.children.iter().any(|c| c.id == node.id));
            let predicate = node.predicate.as_ref().or_else(|| filter_parent.and_then(|p| p.predicate.as_ref()));
            let (Some(object), Some(predicate)) = (&node.object, predicate) else { continue };
            // A partition a run would prune isn't read; a columnstore is read by segments, never sought.
            if node.facts().partition_pruning.is_some()
                || node.facts().storage.as_ref().is_some_and(|s| s.eq_ignore_ascii_case("ColumnStore"))
            {
                continue;
            }
            let Some((schema, table)) = split_object(object) else { continue };
            let rows = scan_rows(snapshot, node, &schema, &table)
                .or_else(|| filter_parent.and(node.act_rows).map(|read| read / node.runs()))
                .unwrap_or(0.0);
            // An estimate is the optimizer's word only where it isn't a default guess: PostgreSQL's for a parameter
            // compared by range, MySQL's fixed 10% for an equality it has no statistics for.
            let guessed = node.facts().selectivity_guess.is_some()
                || (snapshot.server.engine == Engine::Mysql
                    && node.facts().estimated_filtered_percent.is_some_and(|f| {
                        culprit_core::insights::mysql_guessed_filtered(
                            f,
                            culprit_core::insights::condition_count(predicate),
                        )
                    }));
            let kept_share = if let (Some(read), Some(filter)) = (node.act_rows, filter_parent)
                && node.rows_read.is_none()
            {
                filter.act_rows.filter(|_| read > 0.0).map(|kept| (kept / read).min(1.0))
            } else {
                node.kept_share()
            };
            let estimate_keeps_few = if node.act_rows.is_some() {
                kept_share.is_some_and(|kept| kept <= culprit_core::INDEX_KEEPS_AT_MOST)
            } else if node.kind == culprit_core::NodeKind::Seek {
                !guessed && kept_share.is_some_and(|kept| kept <= culprit_core::INDEX_KEEPS_AT_MOST)
            } else {
                !guessed
                    && node.est_rows_total() <= rows * node.est_executions.max(1.0) * culprit_core::INDEX_KEEPS_AT_MOST
            };
            // What the runs measured, where they can say, outweighs the estimate either way.
            let keeps_few = if node.kind == culprit_core::NodeKind::Seek {
                estimate_keeps_few
            } else {
                measured_keeps_few(snapshot, q, rows).unwrap_or(estimate_keeps_few)
            };
            if rows < 10_000.0 || !keeps_few {
                continue;
            }
            let keys = if node.kind == culprit_core::NodeKind::Seek {
                let Some(seek_pred) = node.seek_predicate.as_deref() else { continue };
                let seek_cols = predicate_columns(seek_pred);
                let filter_cols = predicate_columns(predicate);
                let seek_all: Vec<&String> = seek_cols.equality.iter().chain(&seek_cols.range).collect();
                let filter_all: Vec<&String> = filter_cols.equality.iter().chain(&filter_cols.range).collect();
                if seek_all.is_empty()
                    || filter_all.is_empty()
                    || filter_all.iter().all(|fc| seek_all.iter().any(|sc| sc.eq_ignore_ascii_case(fc)))
                {
                    continue;
                }
                let combined = format!("{seek_pred} AND {predicate}");
                let cols = predicate_columns(&combined);
                let mut keys = cols.equality.clone();
                keys.extend(cols.range.into_iter().take(1));
                if keys.is_empty() || leading_index_exists(snapshot, &schema, &table, &keys, &keys) {
                    continue;
                }
                keys
            } else {
                let cols = predicate_columns(predicate);
                let mut keys = cols.equality.clone();
                keys.extend(cols.range.into_iter().take(1));
                if keys.is_empty() || leading_index_exists(snapshot, &schema, &table, &cols.equality, &keys) {
                    continue;
                }
                keys
            };
            match groups
                .iter_mut()
                .find(|g| g.schema == schema && g.table == table && lower_set(&g.keys) == lower_set(&keys))
            {
                Some(g) => {
                    if !g.queries.iter().any(|(x, _)| x.id == q.id) {
                        g.queries.push((q, node.id));
                    }
                }
                None => groups.push(ScanGroup {
                    schema,
                    table,
                    keys,
                    predicate: predicate.clone(),
                    rows,
                    queries: vec![(q, node.id)],
                }),
            }
        }
    }
    groups
}

/// Rows a plan's scan reads. A scan of a filtered index reads only that index's rows, which the plan's estimate of
/// rows read gives; its table cardinality is the whole table's, and would make a scan of an empty filtered index (a
/// queue's "pending" rows, say) look like a read of every row. Any other scan is taken at the table's size: under a
/// TOP, the estimate of rows read assumes matches come early, which is exactly when a costly scan would hide.
pub(crate) fn scan_rows(snapshot: &Snapshot, node: &culprit_core::PlanNode, schema: &str, table: &str) -> Option<f64> {
    if node.kind == culprit_core::NodeKind::Seek {
        return node.rows_read.map(|read| read / node.runs());
    }
    let filtered = node.index.as_deref().is_some_and(|index| {
        let name = index.trim_matches(['[', ']', '"']);
        snapshot.indexes.iter().any(|i| {
            i.schema.eq_ignore_ascii_case(schema)
                && i.table.eq_ignore_ascii_case(table)
                && i.name.eq_ignore_ascii_case(name)
                && i.filter.is_some()
        })
    });
    // Per scan, as the table's size below is: callers count the scans. `rows_read` is already over all of them.
    if filtered {
        return node.rows_read.map(|read| read / node.runs());
    }
    node.table_rows.or_else(|| snapshot.table(schema, table).and_then(|t| t.rows))
}

/// MySQL and MariaDB count rows examined and rows returned per statement. A statement that examines most of the
/// table on every run and returns under a tenth of it filters after reading, whatever the optimizer estimated (MariaDB
/// assumes a condition keeps every row when the column has no statistics). Rows returned measure what the filter kept
/// only when the statement returns every row it keeps (see [`returns_every_kept_row`]): `SELECT COUNT(*) … WHERE
/// created_at > ?` returns one row however many the filter keeps, so there the plan's own estimate decides.
/// `None` where the runs can't say: another engine, or a statement whose rows returned don't measure its filter.
pub(super) fn measured_keeps_few(snapshot: &Snapshot, q: &QueryStat, table_rows: f64) -> Option<bool> {
    if snapshot.server.engine != Engine::Mysql || q.executions <= 0.0 || !returns_every_kept_row(&q.text) {
        return None;
    }
    let examined = q.logical_reads.map(|r| r / q.executions).unwrap_or(0.0);
    let returned = q.rows.map(|r| r / q.executions)?;
    // These totals group runs whose LIMIT values can differ. A literal in a sample or its captured plan establishes
    // no common limit for them, so returned rows cannot establish the filter's selectivity under a LIMIT.
    if culprit_core::sql::has_limit(&q.text, Engine::Mysql) {
        return None;
    }
    (examined >= table_rows * 0.5).then_some(returned <= table_rows * culprit_core::INDEX_KEEPS_AT_MOST)
}

/// Put what the runs measured of each scan's filter on the scan itself ([`culprit_core::plan::MEASURED_KEPT`]), where
/// they can say ([`measured_keeps_few`]): the share of the rows examined that the runs returned. The plan's own rules
/// then judge the scan by it, as `filtered-scan` does, so a scan this check names is never one its plan's viewer is
/// silent about (MySQL's fixed `filtered` guess says nothing; the runs do). A prepare step: it adds what the runs
/// measured, and changes nothing a plan estimated.
pub fn mark_measured_kept(snapshot: &mut Snapshot) {
    if snapshot.server.engine != Engine::Mysql {
        return;
    }
    // A prepared snapshot analyzed again carries the annotation its earlier analysis made. Re-establish it from today's
    // evidence rules, including withdrawing it when a sample LIMIT never described all of the statement's runs.
    fn clear(node: &mut culprit_core::PlanNode) {
        node.facts_mut().measured_kept_share = None;
        node.props.remove(culprit_core::plan::MEASURED_KEPT);
        node.children.iter_mut().for_each(clear);
    }
    for plan in snapshot.workload.iter_mut().flat_map(|w| &mut w.queries).filter_map(|q| q.plan.as_mut()) {
        clear(&mut plan.root);
    }
    let mut marks: Vec<(String, u32, f64)> = Vec::new();
    for q in snapshot.queries() {
        let Some(plan) = &q.plan else { continue };
        let scans: Vec<&culprit_core::PlanNode> =
            plan.nodes().into_iter().filter(|n| n.kind == culprit_core::NodeKind::Scan && n.object.is_some()).collect();
        // One table read whole: rows returned measure that scan's filter only then.
        let [scan] = scans.as_slice() else { continue };
        if scan.act_rows.is_some() || scan.predicate.is_none() {
            continue;
        }
        let Some((schema, table)) = scan.object.as_deref().and_then(split_object) else { continue };
        let Some(rows) = scan_rows(snapshot, scan, &schema, &table) else { continue };
        if measured_keeps_few(snapshot, q, rows).is_none() {
            continue;
        }
        let (Some(examined), Some(returned)) = (q.reads_per_execution(), q.rows_per_execution()) else { continue };
        marks.push((q.id.clone(), scan.id, (returned / examined.max(1.0)).clamp(0.0, 1.0)));
    }
    let Some(workload) = snapshot.workload.as_mut() else { return };
    for (id, node, kept) in marks {
        let Some(plan) = workload.queries.iter_mut().find(|q| q.id == id).and_then(|q| q.plan.as_mut()) else {
            continue;
        };
        fn mark(n: &mut culprit_core::PlanNode, id: u32, kept: f64) {
            if n.id == id {
                n.facts_mut().measured_kept_share = Some(kept);
                n.props.insert(culprit_core::plan::MEASURED_KEPT.into(), kept.to_string());
            }
            n.children.iter_mut().for_each(|c| mark(c, id, kept));
        }
        mark(&mut plan.root, node, kept);
    }
}

/// Whether a statement returns every row its filter keeps, so that rows returned measure the filter: a SELECT from one
/// table, without aggregates, GROUP BY, DISTINCT, TOP or FETCH, HAVING, a set operation or a subquery. Any of those
/// returns fewer rows than the filter keeps, and a join can return fewer or more. A LIMIT prevents the statement's
/// returned-row totals from measuring its filter ([`measured_keeps_few`]).
pub(super) fn returns_every_kept_row(text: &str) -> bool {
    const AGGREGATES: [&str; 14] = [
        "COUNT",
        "SUM",
        "AVG",
        "MIN",
        "MAX",
        "GROUP_CONCAT",
        "JSON_ARRAYAGG",
        "JSON_OBJECTAGG",
        "STD",
        "STDDEV",
        "STDDEV_POP",
        "STDDEV_SAMP",
        "VARIANCE",
        "BIT_OR",
    ];
    let plain = text.replace(['`', '"', '[', ']'], "").to_ascii_uppercase();
    let words: Vec<&str> =
        plain.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    let calls = |name: &str| {
        plain.match_indices(name).any(|(at, _)| {
            let before = plain[..at].chars().next_back();
            let after = plain[at + name.len()..].trim_start();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') && after.starts_with('(')
        })
    };
    words.first() == Some(&"SELECT")
        && words.iter().filter(|w| **w == "SELECT").count() == 1
        && !words.iter().any(|w| {
            matches!(*w, "GROUP" | "DISTINCT" | "TOP" | "FETCH" | "HAVING" | "UNION" | "INTERSECT" | "EXCEPT" | "JOIN")
        })
        && !AGGREGATES.iter().any(|a| calls(a))
        && culprit_core::sql::tables_in(text, Engine::Mysql).len() == 1
}

/// An index on a scan's first key that a statement in the workload created and the catalog doesn't hold now.
pub(super) struct DroppedIndex {
    /// The key and the index's name, as the statement wrote them (without brackets or quotes, in lowercase).
    key: String,
    name: String,
    /// The statement, as far as 200 characters, and where the workload recorded it.
    statement: String,
    source: String,
    first_seen: Option<String>,
}

/// The index led by `first_key` that a statement in the workload created (`CREATE [UNIQUE] [NONCLUSTERED] INDEX
/// [CONCURRENTLY] [IF NOT EXISTS] <name> ON <table> (<first key>, …)`) and the catalog doesn't hold now: it existed,
/// and was dropped. `None` when the workload shows no such statement, or the index is still there.
fn dropped_index(snapshot: &Snapshot, schema: &str, table: &str, first_key: &str) -> Option<DroppedIndex> {
    let clean = |w: &str| w.replace(['[', ']', '"', '`'], "").to_ascii_lowercase();
    let (schema, table, first_key) = (schema.to_ascii_lowercase(), table.to_ascii_lowercase(), clean(first_key));
    let source = snapshot.workload.as_ref()?.source.clone();
    snapshot.queries().iter().find_map(|q| {
        let text = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let (head, columns) = text.split_once('(')?;
        let words: Vec<String> = head.split_whitespace().map(clean).collect();
        let index_at =
            words.iter().position(|w| w == "index").filter(|_| words.first().is_some_and(|w| w == "create"))?;
        let on_at = words.iter().skip(index_at).position(|w| w == "on")? + index_at;
        let name = words.get(on_at - 1).filter(|_| on_at > index_at + 1)?;
        let object = words[on_at + 1..].iter().find(|w| *w != "only")?;
        let (in_schema, in_table) = object.rsplit_once('.').map_or((None, object.as_str()), |(s, t)| (Some(s), t));
        let first = clean(columns.split([',', ')']).next()?.split_whitespace().next()?);
        let same = in_table == table && in_schema.is_none_or(|s| s.rsplit('.').next() == Some(schema.as_str()));
        let kept = snapshot.indexes.iter().any(|i| i.table.eq_ignore_ascii_case(&table) && clean(&i.name) == *name);
        (same && first == first_key && !kept).then(|| DroppedIndex {
            key: first_key.clone(),
            name: name.clone(),
            statement: text.chars().take(200).collect(),
            source: source.clone(),
            first_seen: q.first_seen.clone(),
        })
    })
}

pub(crate) fn split_object(object: &str) -> Option<(String, String)> {
    let clean = object.replace(['[', ']', '"'], "");
    let parts: Vec<&str> = clean.split('.').collect();
    match parts.as_slice() {
        [.., schema, table] => Some((schema.to_string(), table.to_string())),
        [table] => Some(("dbo".to_string(), table.to_string())),
        _ => None,
    }
}

/// A disabled index that would lead with the scan's columns ([`leading_index`]'s rule): the fix is to bring it back,
/// not to build its twin.
pub(crate) fn disabled_leading_index<'a>(snapshot: &'a Snapshot, g: &ScanGroup) -> Option<&'a IndexStat> {
    let cols = predicate_columns(&g.predicate);
    let wanted: &[String] = if g.keys.len() > cols.equality.len() + cols.range.len().min(1) {
        &g.keys
    } else if cols.equality.is_empty() {
        g.keys.get(..1)?
    } else {
        &cols.equality
    };
    snapshot.indexes.iter().find(|i| {
        let existing = i.key_names();
        i.schema.eq_ignore_ascii_case(&g.schema)
            && i.can_seek(&cols.equality)
            && i.table.eq_ignore_ascii_case(&g.table)
            && i.is_disabled
            && existing.len() >= wanted.len()
            && lower_set(&existing[..wanted.len()].iter().map(|s| s.to_string()).collect::<Vec<_>>())
                == lower_set(wanted)
    })
}

/// An index already leads with the equality columns: the scan has another cause (type mismatch, function on the column, stale stats).
pub(super) fn leading_index_exists(
    snapshot: &Snapshot,
    schema: &str,
    table: &str,
    equality: &[String],
    keys: &[String],
) -> bool {
    leading_index(snapshot, schema, table, equality, keys).is_some()
}

/// The index that leads with the equality columns (or, without any, with the first key), in any order.
pub(crate) fn leading_index<'a>(
    snapshot: &'a Snapshot,
    schema: &str,
    table: &str,
    equality: &[String],
    keys: &[String],
) -> Option<&'a IndexStat> {
    let wanted = if equality.is_empty() { keys.get(..1)? } else { equality };
    snapshot.indexes.iter().find(|i| {
        let existing = i.key_names();
        i.schema.eq_ignore_ascii_case(schema)
            && i.can_seek(equality)
            && i.table.eq_ignore_ascii_case(table)
            && !i.is_disabled
            && existing.len() >= wanted.len()
            && lower_set(&existing[..wanted.len()].iter().map(|s| s.to_string()).collect::<Vec<_>>())
                == lower_set(wanted)
    })
}

/// A full scan of a large table that keeps few rows by a filter no index leads with, and what its finding concludes.
pub struct FilteredScan<'s> {
    group: ScanGroup<'s>,
    impact: QueryImpact,
    severity: Severity,
    confidence: Confidence,
    /// The statements stopped (or ran once): told in the past.
    history: bool,
    /// A disabled index that would lead with the keys: the fix brings it back.
    disabled: Option<&'s IndexStat>,
    /// The foreign key whose columns the keys are.
    foreign_key: Option<&'s culprit_core::ForeignKey>,
    /// The table is no longer in the catalog: its plans speak for it.
    gone: bool,
    /// An index on the first key that the workload shows created, and the catalog doesn't hold now.
    dropped: Option<DroppedIndex>,
    /// The columns the scan's own index requests ask the index to carry, less those too large to.
    include: Vec<String>,
    large: Option<LargeIncludes>,
}

fn filtered_scans(snapshot: &Snapshot) -> Vec<FilteredScan<'_>> {
    scan_groups(snapshot).into_iter().map(|g| decide(snapshot, g)).collect()
}

fn decide<'s>(snapshot: &'s Snapshot, g: ScanGroup<'s>) -> FilteredScan<'s> {
    let queries: Vec<&QueryStat> = g.queries.iter().map(|(q, _)| *q).collect();
    let impact = query_impact(snapshot, &queries).expect("queries have workload");
    // Graded on the scan's part of each statement, by its share of the plan's cost: a 12K-row lookup table scanned by
    // a heavy star join is a sliver of that join's time. A plan without costs (SQLite) gives the statement's.
    let time_ms: f64 = g
        .queries
        .iter()
        .map(|(q, node)| {
            let part = q.plan.as_ref().filter(|p| p.weighed()).and_then(|p| {
                let total = p.total_cost.max(p.root.subtree_cost);
                let n = p.nodes().into_iter().find(|n| n.id == *node)?;
                (total > 0.0).then(|| (n.subtree_cost.max(n.self_cost) / total).clamp(0.0, 1.0))
            });
            q.total_time_ms * part.unwrap_or(1.0)
        })
        .sum();
    let total = snapshot.workload.as_ref().map_or(0.0, |w| w.total_time_ms);
    let scan_share = if total > 0.0 { (time_ms / total).min(impact.share) } else { impact.share };
    let severity = severity_for_time_of(snapshot, &queries, time_ms, scan_share);
    // Statements that stopped (or ran once) are told in the past, all of it: title and summary decide it once. A table
    // that is gone has no catalog left to say which indexes it had, so only the plans speak for it.
    let history = queries.iter().all(|q| crate::query_is_history(snapshot, q));
    let disabled = disabled_leading_index(snapshot, &g);
    let foreign_key = snapshot.foreign_keys.iter().find(|fk| {
        fk.schema.eq_ignore_ascii_case(&g.schema)
            && fk.table.eq_ignore_ascii_case(&g.table)
            && !fk.columns.is_empty()
            && fk.columns.iter().all(|c| g.keys.iter().any(|k| k.eq_ignore_ascii_case(c)))
    });
    // A table the catalog no longer lists: the plans read it before it was dropped or renamed.
    let gone = table_gone(snapshot, &g.table, &queries);
    // An index on the key that the workload shows being created, and the catalog doesn't hold now: it was dropped.
    let dropped = g.keys.first().and_then(|key| dropped_index(snapshot, &g.schema, &g.table, key));
    // A scan's own index request also says which returned columns the index must carry to avoid lookups. Keep those
    // columns when the filtered-scan finding absorbs the plan's request.
    let mut include = Vec::new();
    for hint in queries.iter().filter_map(|q| q.plan.as_ref()).flat_map(|p| &p.index_hints) {
        let keys: Vec<_> = hint.equality.iter().chain(&hint.inequality).cloned().collect();
        if names_table(&hint.table, &g.schema, &g.table) && lower_set(&keys) == lower_set(&g.keys) {
            for column in &hint.include {
                if !g.keys.iter().chain(&include).any(|k| k.eq_ignore_ascii_case(column)) {
                    include.push(column.clone());
                }
            }
        }
    }
    let large = large_includes(snapshot, &g.schema, &g.table, &mut include);
    FilteredScan {
        group: g,
        impact,
        severity,
        confidence: Confidence::High,
        history,
        disabled,
        foreign_key,
        gone,
        dropped,
        include,
        large,
    }
}

fn filtered_scan_finding(snapshot: &Snapshot, s: FilteredScan<'_>) -> Finding {
    let g = &s.group;
    let table_name = format!("{}.{}", g.schema, g.table);
    let key_text = format::list(&g.keys);
    let queries: Vec<&QueryStat> = g.queries.iter().map(|(q, _)| *q).collect();
    let n = queries.len().to_string();
    let tense = if s.history { "past" } else { "present" };
    let mut f = finding(
        "filtered-scan",
        &format!("{table_name}({})", g.keys.join(",")),
        Category::Indexes,
        subject(SubjectKind::Table, &table_name),
    );
    f.facts.index_keys = g.keys.clone();
    apply_query_impact(&mut f, &s.impact);
    f.severity = s.severity;
    f.confidence = s.confidence;
    f.title = t!("check.filtered_scan.title", table = &g.table, tense = tense, keys = &key_text);
    if s.gone {
        f.actionability = culprit_core::Actionability::History;
    }
    let mut summary = vec![
        t!(
            "check.filtered_scan.summary.reads",
            tense = tense,
            n = &n,
            table = &table_name,
            predicate = g.predicate.trim()
        ),
        g.row_basis(snapshot),
        match s.disabled {
            Some(i) => t!("check.filtered_scan.summary.disabled", index = &i.name, keys = &key_text, tense = tense),
            None if s.gone => t!("check.filtered_scan.summary.gone_plans", n = &n, keys = &key_text),
            None => t!("check.filtered_scan.summary.no_index", keys = &key_text, tense = tense),
        },
        t!("check.filtered_scan.summary.share", n = &n, tense = tense, share = format::percent(s.impact.share)),
    ];
    if let Some(fk) = s.foreign_key {
        summary.push(t!("check.filtered_scan.summary.foreign_key", keys = &key_text, parent = &fk.ref_table));
    }
    if s.gone {
        summary.push(t!("check.filtered_scan.summary.gone", table = &table_name));
    }
    if let Some(d) = &s.dropped {
        let (dated, when) = match &d.first_seen {
            Some(at) => ("yes", crate::queries::when_phrase(at)),
            None => ("no", String::new()),
        };
        summary.push(if s.gone {
            t!(
                "check.filtered_scan.summary.dropped_with_table",
                key = &d.key,
                source = &d.source,
                statement = &d.statement,
                dated = dated,
                when = &when
            )
        } else {
            t!(
                "check.filtered_scan.summary.dropped",
                key = &d.key,
                source = &d.source,
                statement = &d.statement,
                dated = dated,
                when = &when,
                index = &d.name
            )
        });
    }
    f.summary = summary.join(" ");
    f.queries = g
        .queries
        .iter()
        .map(|(q, node)| culprit_core::QueryRef { id: q.id.clone(), plan_node: Some(*node), note: None })
        .collect();
    f.metrics.push(Metric::new(t!("common.metric.share_of_db_time"), s.impact.share, Unit::Ratio));
    if let Some(per_day_ms) = s.impact.per_day_ms {
        f.metrics.push(Metric::new(t!("common.metric.db_time_per_day"), per_day_ms, Unit::Ms));
    }
    f.metrics.push(Metric::new(t!("check.filtered_scan.metric.size_basis"), g.rows, Unit::Rows));
    f.metrics.push(Metric::new(t!("check.filtered_scan.metric.queries"), queries.len() as f64, Unit::Count));
    if let Some(large) = &s.large {
        f.caveats.push(large.note(&g.schema, &g.table));
    }
    f.fix = Some(match s.disabled {
        Some(i) => widen_fix(snapshot, i, &s.include),
        None => new_index_fix(
            snapshot,
            &g.schema,
            &g.table,
            &g.keys,
            &s.include,
            &queries,
            t!("check.filtered_scan.fix", keys = &key_text, n = &n),
        ),
    });
    f.verify = if s.gone {
        f.facts.nothing_to_check = Some(culprit_core::NothingToCheck::Gone);
        vec![t!("check.filtered_scan.verify.gone", table = &table_name)]
    } else {
        vec![
            match s.disabled {
                Some(i) => t!("check.filtered_scan.verify.plan_index", n = &n, table = &g.table, index = &i.name),
                None => t!("check.filtered_scan.verify.plan_new", n = &n, table = &g.table),
            },
            // On running totals the share holds every run from before the index: What changed shows the runs since.
            match crate::queries::what_changed_between_scans(
                snapshot,
                &t!("check.filtered_scan.verify.queries", n = &n),
                &t!("check.filtered_scan.verify.compare", n = &n),
            ) {
                Some(between) => [between, t!("check.filtered_scan.verify.counters")].join(" "),
                None => t!("check.filtered_scan.verify.rescan"),
            },
        ]
    };
    f
}
