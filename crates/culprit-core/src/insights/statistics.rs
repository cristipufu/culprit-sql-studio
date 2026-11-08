//! Plan insights about the optimizer's estimates: missing or stale statistics, and steps whose rows were far from
//! the estimate.

use super::*;

pub(super) fn misestimate(cx: &Cx) -> Vec<Draft> {
    // The plan's misestimates by the one shared rule (`PlanNode::misestimates`), judged here again so a plan saved
    // before that rule reads the same as a fresh one.
    let found: HashMap<u32, f64> = cx.plan.root.misestimates().into_iter().collect();
    // The deepest misestimated operators are the causes; everything above inherits their error.
    let causes: Vec<&PlanNode> = cx
        .nodes
        .iter()
        .filter(|n| found.contains_key(&n.id))
        .filter(|n| {
            let mut below = Vec::new();
            n.children.iter().for_each(|c| c.walk(&mut below));
            !below.iter().any(|b| found.contains_key(&b.id))
        })
        // A table variable estimated at one row is the table-variable rule's: no statistics to refresh.
        .filter(|n| !(table_variable_of(n).is_some() && n.est_rows_total() <= 1.5))
        .copied()
        .collect();
    let mut causes = causes;
    causes.sort_by(|a, b| {
        let w = |n: &PlanNode| found[&n.id].abs() * rows_out(n).max(n.est_rows_for_runs()).log10().max(1.0);
        w(b).total_cmp(&w(a))
    });
    causes
        .into_iter()
        .take(2)
        .map(|n| {
            let factor = found[&n.id];
            let est = n.est_rows_for_runs();
            let act = rows_out(n);
            // MySQL's EXPLAIN ANALYZE estimates the filter in a Filter node above the table it reads.
            let table = n.object.clone().or_else(|| (n.op == "Filter").then(|| first_object(n)).flatten());
            let subject = table.clone().unwrap_or_else(|| n.op.clone());
            let mut above = 0.0;
            // A sort or hash above that ran out of memory was sized for the wrong number of rows.
            let mut spill_above = false;
            // What was planned from this estimate: joins, sorts, aggregates, spools, parallelism above it.
            let mut consumers = false;
            for p in cx.ancestors(n) {
                above += cx.share(p);
                spill_above |= p.warnings.contains(&PlanWarning::Spill);
                consumers |= matches!(
                    p.kind,
                    NodeKind::Join | NodeKind::Sort | NodeKind::Aggregate | NodeKind::Spool | NodeKind::Parallelism
                ) || p.op.contains("Hash");
            }
            let impact = (cx.subtree(n) + above).min(1.0);
            let mut d = Draft::new("misestimate", InsightArea::Statistics, impact, vec![n.id]);
            d.floor = if factor.abs() >= 100.0 { Severity::Medium } else { Severity::Low };
            if !consumers {
                // Nothing in this plan was sized from it; the statistics still decide other plans on the table.
                d.floor = Severity::Low;
                d.cap = Severity::Low;
            }
            let (many, count, from) = (plural_n(est), rows_text(est), table_leaf(&subject));
            let (got, times) = (rows_text(act), rows_text(factor.abs()));
            d.title = if factor >= 0.0 {
                t!("insight.misestimate.title_more", n = many, count = count, table = from, got = got, factor = times)
            } else {
                t!("insight.misestimate.title_fewer", n = many, count = count, table = from, got = got, factor = times)
            };
            let cond = conditions(n);
            // Multi-column statistics only describe plain columns; a filter on an expression (id % 4, lower(x)) needs
            // expression statistics instead, so the advice changes.
            let on_expression = cond.split([' ', '(', ')']).any(|t| matches!(t, "%" | "+" | "-" | "*" | "/"))
                || wrapped_column(&cond, cx.engine()).is_some();
            let cols: Vec<String> = if on_expression { Vec::new() } else { filter_columns(&cond) };
            let mut detail = vec![if !consumers {
                t!("insight.misestimate.detail.unused")
            } else if spill_above {
                t!("insight.misestimate.detail.planned_spilled")
            } else {
                t!("insight.misestimate.detail.planned")
            }];
            if cols.len() >= 2 {
                detail.push(t!("insight.misestimate.detail.correlated", columns = format::list(&cols)));
            } else if on_expression {
                detail.push(t!("insight.misestimate.detail.expression"));
            }
            d.detail = detail.join(" ");
            let mysql_histograms = !cx.plan.facts().server_version.as_deref()
                .is_some_and(|v| !v.contains("MariaDB") && !crate::version_at_least(v, [8, 0, 0]));
            let (fix, sql) = match (cx.engine(), &table) {
                // Table variables have no statistics, and a temporary table's belong to the session that filled it:
                // nothing Culprit could hand over to run would reach them.
                (Engine::Mssql, Some(t)) if table_variable_of(n).is_some() => {
                    (t!("insight.misestimate.fix.table_variable", table = table_leaf(t)), None)
                }
                (Engine::Mssql, Some(t)) if table_leaf(t).starts_with('#') => {
                    (t!("insight.misestimate.fix.temporary_table", table = table_leaf(t)), None)
                }
                (Engine::Mssql, Some(t)) => (
                    if cols.len() >= 2 {
                        t!("insight.misestimate.fix.mssql_columns", table = table_leaf(t))
                    } else {
                        t!("insight.misestimate.fix.mssql", table = table_leaf(t))
                    },
                    Some(if cols.len() >= 2 {
                        format!("UPDATE STATISTICS {} WITH FULLSCAN;\nCREATE STATISTICS st_{}_{} ON {} ({});", qualified(Engine::Mssql, t), table_leaf(t).to_ascii_lowercase(), cols.join("_").to_ascii_lowercase(), qualified(Engine::Mssql, t), cols.iter().map(|c| quote(Engine::Mssql, c)).collect::<Vec<_>>().join(", "))
                    } else {
                        format!("UPDATE STATISTICS {} WITH FULLSCAN;", qualified(Engine::Mssql, t))
                    }),
                ),
                (Engine::Yugabyte, Some(t)) => (
                    t!("insight.misestimate.fix.yugabyte", table = table_leaf(t)),
                    Some(Engine::Yugabyte.dialect().refresh_statistics(&qualified(Engine::Yugabyte, t))),
                ),
                (Engine::Postgres, Some(t)) => (
                    if cols.len() >= 2 {
                        t!("insight.misestimate.fix.postgres_columns", table = table_leaf(t))
                    } else if on_expression {
                        t!("insight.misestimate.fix.postgres_expression", table = table_leaf(t))
                    } else {
                        t!("insight.misestimate.fix.postgres", table = table_leaf(t))
                    },
                    Some(if cols.len() >= 2 {
                        format!("ANALYZE {};\nCREATE STATISTICS {}_{}_stats (dependencies, mcv) ON {} FROM {};\nANALYZE {};", qualified(Engine::Postgres, t), table_leaf(t).to_ascii_lowercase(), cols.join("_").to_ascii_lowercase(), cols.iter().map(|c| quote(Engine::Postgres, c)).collect::<Vec<_>>().join(", "), qualified(Engine::Postgres, t), qualified(Engine::Postgres, t))
                    } else {
                        Engine::Postgres.dialect().refresh_statistics(&qualified(Engine::Postgres, t))
                    }),
                ),
                // MariaDB collects histograms with its engine-independent statistics; MySQL has them from 8.0.
                (Engine::Mysql, Some(t)) if cx.mariadb() && !cols.is_empty() => (
                    t!("insight.misestimate.fix.mariadb_columns", table = table_leaf(t), columns = format::list(&cols)),
                    Some(format!("ANALYZE TABLE {} PERSISTENT FOR COLUMNS ({}) INDEXES ();", qualified(Engine::Mysql, t), cols.iter().map(|c| quote(Engine::Mysql, c)).collect::<Vec<_>>().join(", "))),
                ),
                (Engine::Mysql, Some(t)) if mysql_histograms && !cols.is_empty() => (
                    t!("insight.misestimate.fix.mysql_columns", table = table_leaf(t), columns = format::list(&cols)),
                    Some(format!("ANALYZE TABLE {} UPDATE HISTOGRAM ON {};", qualified(Engine::Mysql, t), cols.iter().map(|c| quote(Engine::Mysql, c)).collect::<Vec<_>>().join(", "))),
                ),
                (Engine::Mysql, Some(t)) => (
                    t!("insight.misestimate.fix.refresh", table = table_leaf(t)),
                    Some(Engine::Mysql.dialect().refresh_statistics(&qualified(Engine::Mysql, t))),
                ),
                (Engine::Sqlite, Some(t)) => (
                    t!("insight.misestimate.fix.refresh", table = table_leaf(t)),
                    Some(Engine::Sqlite.dialect().refresh_statistics(&qualified(Engine::Sqlite, t))),
                ),
                (_, None) => (t!("insight.misestimate.fix.below"), None),
            };
            d.fix = fix;
            d.fix_sql = sql;
            d.verify = t!("insight.misestimate.verify");
            d
        })
        .collect()
}

pub(super) fn missing_statistics(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        .filter(|n| warns_of_no_statistics(n))
        .take(1)
        .map(|n| {
            let columns = &n.facts().columns_without_statistics;
            let mut d = Draft::new("missing-statistics", InsightArea::Statistics, cx.subtree(n), vec![n.id]);
            d.cap = Severity::Low;
            d.title = t!(
                "insight.missing_statistics.title",
                named = known(!columns.is_empty()),
                columns = columns.join(", ")
            );
            d.detail = match keys_without_statistics(cx, n) {
                Some(context) => stale_index_statistics_detail(&context, None),
                None => t!("insight.missing_statistics.detail"),
            };
            d.fix = t!("insight.missing_statistics.fix");
            d.fix_sql = Some("SELECT name, is_auto_create_stats_on, is_read_only\n  FROM sys.databases\n WHERE database_id = DB_ID();".into());
            d.verify = t!("insight.missing_statistics.verify");
            d
        })
        .collect()
}

/// An operator SQL Server warned has no statistics on some columns. A table variable never has statistics; the
/// table-variable rule says what to do about it.
fn warns_of_no_statistics(n: &PlanNode) -> bool {
    n.warnings.contains(&PlanWarning::MissingStatistics) && table_variable_of(n).is_none()
}

/// Seek-key context for ColumnsWithNoStatistics. The legacy name does not imply a current staleness verdict: a
/// composite index has a histogram only on its first key, and a saved warning describes compilation time.
#[derive(Clone, Debug, PartialEq)]
pub struct StaleIndexStatistics {
    /// The table, as the plan names it (`app.support_escalations`).
    pub table: String,
    /// The columns SQL Server named.
    pub columns: Vec<String>,
    /// The indexes they are keys of.
    pub indexes: Vec<String>,
    /// The rows the table held when the plan was compiled (SQL Server's TableCardinality), where the plan says.
    pub table_rows: Option<f64>,
}

/// The index keys a plan warns have no statistics, if every column it names is one (see [`StaleIndexStatistics`]).
pub fn stale_index_statistics(plan: &Plan) -> Option<StaleIndexStatistics> {
    let cx = Cx::new(plan);
    let n = cx.nodes.iter().copied().find(|n| warns_of_no_statistics(n))?;
    keys_without_statistics(&cx, n)
}

fn keys_without_statistics(cx: &Cx, n: &PlanNode) -> Option<StaleIndexStatistics> {
    let table = n.object.clone()?;
    let columns: Vec<String> =
        n.facts().columns_without_statistics.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    if columns.is_empty() {
        return None;
    }
    let mut indexes = columns.iter().map(|c| index_seeked_on(cx, &table, c)).collect::<Option<Vec<_>>>()?;
    indexes.dedup();
    let leaf = table_leaf(&table);
    let table_rows = n.table_rows.or_else(|| {
        cx.nodes
            .iter()
            .filter(|m| m.object.as_deref().is_some_and(|o| table_leaf(o).eq_ignore_ascii_case(&leaf)))
            .find_map(|m| m.table_rows)
    });
    Some(StaleIndexStatistics { table, columns, indexes, table_rows })
}

/// Context for a compilation-time warning. A seek key proves an index exists, not that this column leads its
/// histogram, that its statistics were stale then, or that any current statistics need updating.
pub fn stale_index_statistics_detail(s: &StaleIndexStatistics, catalog: Option<&str>) -> String {
    let mut detail = vec![t!(
        "insight.missing_statistics.seek_keys",
        columns = format::list(&s.columns),
        indexes = format::list(&s.indexes)
    )];
    detail.extend(catalog.map(|said| t!("insight.missing_statistics.catalog", said = said.trim_end_matches('.'))));
    detail.join(" ")
}

/// The index of `table` the plan seeks on `column`, which makes the column that index's key. SQL Server writes each of a
/// seek's conditions key first: `[id] = [db].[app].[orders].[id]`.
fn index_seeked_on(cx: &Cx, table: &str, column: &str) -> Option<String> {
    let leaf = table_leaf(table);
    cx.nodes.iter().find_map(|n| {
        let same_table = n.object.as_deref().is_some_and(|o| table_leaf(o).eq_ignore_ascii_case(&leaf));
        let keyed = n.seek_predicate.as_deref().is_some_and(|seek| {
            top_level_and(seek)
                .into_iter()
                .any(|part| identifiers(part).first().is_some_and(|k| k.eq_ignore_ascii_case(column)))
        });
        n.index.as_deref().filter(|_| same_table && keyed).map(table_leaf)
    })
}

pub(super) fn index_ignored(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Mysql {
        return Vec::new();
    }
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| n.op == "Table scan" && index_could_pay(cx, n)) {
        let keys = possible_keys_for_filter(cx, n);
        if keys.is_empty() {
            continue;
        }
        let kept = filter_keeps(cx, n).unwrap_or(1.0);
        let mut d = Draft::new("index-ignored", InsightArea::Statistics, cx.share(n), vec![n.id]);
        d.cap = Severity::Medium;
        d.title = t!("insight.index_ignored.title", keys = keys.join(", "), table = table_leaf(&what(n)));
        let (kept, rows, indexes) = (format::percent(kept), format::count(rows_read(n)), keys.len());
        d.detail = if n.act_rows.is_some() {
            t!("insight.index_ignored.detail_kept", kept = kept, rows = rows, indexes = indexes)
        } else {
            t!("insight.index_ignored.detail_expected", kept = kept, rows = rows, indexes = indexes)
        };
        d.fix = t!("insight.index_ignored.fix");
        d.fix_sql = n.object.as_ref().map(|t| Engine::Mysql.dialect().refresh_statistics(&qualified(Engine::Mysql, t)));
        d.verify = t!("insight.index_ignored.verify");
        out.push(d);
    }
    out
}
