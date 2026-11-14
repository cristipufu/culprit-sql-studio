//! Plan insights about how a statement is written: predicates an index can't use, conversions, joins, loops and
//! functions the plan runs row by row.

use super::*;

pub(super) fn implicit_conversion(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Mssql {
        return Vec::new();
    }
    // Only the warnings about a converted column the table still has (see `current_conversions`); older scans flagged
    // every conversion, and a plan captured before a column changed type still shows its old conversion.
    let plan_facts = cx.plan.facts();
    let conversions: Vec<&crate::PlanConversion> =
        plan_facts.conversions.iter().filter(|c| !current_conversions(cx.plan, &c.expression).is_empty()).collect();
    let plan_lines: Vec<String> = conversions.iter().map(|c| c.text()).collect();
    let node = cx
        .nodes
        .iter()
        // A converted column is the problem; a converted parameter, variable or literal costs nothing.
        .filter(|n| !current_conversions(cx.plan, &conditions(n)).is_empty())
        .max_by(|a, b| a.self_cost.total_cmp(&b.self_cost))
        .copied();
    if plan_lines.is_empty() && node.is_none() {
        return Vec::new();
    }
    let seek_line = conversions.iter().find(|c| c.issue == crate::PlanConversionIssue::SeekPlan).map(|c| c.text());
    let text =
        seek_line.clone().or_else(|| plan_lines.first().cloned()).or_else(|| node.map(conditions)).unwrap_or_default();
    // What the conversion costs is SQL Server's own verdict, never inferred from the plan's shape: a scan may have no
    // index on the column to seek, or be the right plan for a small table. "Seek Plan" means it kept SQL Server from
    // seeking an index; "Cardinality Estimate" that it swayed the row estimates; no warning, that it changed nothing.
    // SQL Server words it "may affect": an operator that seeks all the same (a range through the conversion) wasn't
    // stopped.
    let blocks_seek = seek_line.is_some() && node.is_none_or(|n| n.kind != NodeKind::Seek);
    let converted = current_conversions(cx.plan, &text).into_iter().next();
    let column = converted.as_ref().map(|(c, _)| c.clone()).or_else(|| convert_column(&text));
    let target = converted.map(|(_, t)| t);
    let impact = node.map_or(0.05, |n| cx.share(n));
    let mut d =
        Draft::new("implicit-conversion", InsightArea::Query, impact, node.map(|n| vec![n.id]).unwrap_or_default());
    // The column and the type it is converted to, where the plan names them.
    let (named, col) = (known(column.is_some()), column.clone().unwrap_or_default());
    let (typed, to) = (known(target.is_some()), target.clone().unwrap_or_default());
    let expression = format::predicate(&convert_expression(&text));
    if blocks_seek {
        d.floor = Severity::Medium;
        d.title = t!("insight.implicit_conversion.title.blocks_seek", named = named, column = &col);
        d.detail = t!(
            "insight.implicit_conversion.detail.blocks_seek",
            named = named,
            column = &col,
            typed = typed,
            target = &to,
            expression = &expression
        );
    } else if !plan_lines.is_empty() {
        d.cap = Severity::Low;
        d.title = t!("insight.implicit_conversion.title.estimates", named = named, column = &col);
        d.detail = t!(
            "insight.implicit_conversion.detail.estimates",
            named = named,
            column = &col,
            typed = typed,
            target = &to,
            expression = &expression
        );
    } else {
        d.cap = Severity::Low;
        d.title = t!("insight.implicit_conversion.title.per_row", named = named, column = &col);
        d.detail = t!(
            "insight.implicit_conversion.detail.per_row",
            named = named,
            column = &col,
            typed = typed,
            target = &to,
            expression = &expression
        );
    }
    d.fix = mismatch_fix(column.as_deref(), target.as_deref());
    d.verify = if blocks_seek {
        t!("insight.implicit_conversion.verify_seek")
    } else {
        t!("insight.implicit_conversion.verify", named = named, column = &col)
    };
    vec![d]
}

/// `CONVERT_IMPLICIT(nvarchar(256),[db].[dbo].[Users].[Email],0)` → "Users.Email".
pub(super) fn convert_column(text: &str) -> Option<String> {
    let start = text.find("CONVERT_IMPLICIT(")? + "CONVERT_IMPLICIT(".len();
    let rest = &text[start..];
    let after_type = rest.find("),").map(|i| &rest[i + 2..]).or_else(|| rest.split_once(',').map(|(_, r)| r))?;
    let expr = after_type.split(',').next()?;
    let parts: Vec<String> = expr.split('.').map(clean_identifier).collect();
    match parts.as_slice() {
        [.., table, column] if !column.is_empty() && !column.starts_with('@') => Some(format!("{table}.{column}")),
        [column] if !column.is_empty() && !column.starts_with('@') => Some(column.clone()),
        _ => None,
    }
}

pub(super) fn convert_expression(text: &str) -> String {
    text.split_once(": ").map(|(_, e)| e.to_string()).unwrap_or_else(|| text.to_string())
}

pub(super) fn non_sargable(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| index_could_pay(cx, n) || seek_filter_could_pay(cx, n)) {
        let Some(filter) = scan_filter_text(cx, n) else { continue };
        let Some(Blocker::Function(func, col)) = blocker(cx, n, &filter) else { continue };
        let table = what(n);
        let impact = cx.share(n).max(cx.subtree(n) * 0.5);
        let mut d = Draft::new("non-sargable", InsightArea::Query, impact, vec![n.id]);
        let cast = func.starts_with("::") || func == "CAST" || func == "CONVERT";
        let leaf = table_leaf(&table);
        d.title = if cast {
            t!("insight.non_sargable.title_cast", column = &col, table = &leaf)
        } else {
            t!("insight.non_sargable.title", function = &func, column = &col, table = &leaf)
        };
        let read = rows_read(n);
        let mut detail = vec![t!(
            "insight.non_sargable.detail",
            column = &col,
            filter = format::predicate(&filter),
            op = &n.op,
            n = read,
            rows = format::count(read),
            kept = format::percent(filter_keeps(cx, n).unwrap_or(1.0))
        )];
        detail.extend(cx.work_sentence(impact, &[n.id]));
        d.detail = detail.join(" ");
        let lower = func.to_ascii_lowercase();
        d.fix = if matches!(
            lower.as_str(),
            "year"
                | "month"
                | "day"
                | "date"
                | "datepart"
                | "date_trunc"
                | "date_part"
                | "extract"
                | "date_format"
                | "to_char"
        ) || lower == "::date"
        {
            t!("insight.non_sargable.fix.date_range", column = &col)
        } else if matches!(lower.as_str(), "isnull" | "ifnull" | "coalesce" | "nvl") {
            t!("insight.non_sargable.fix.null_replacement", column = &col)
        } else if cast {
            t!("insight.non_sargable.fix.cast", column = &col)
        } else {
            match cx.engine() {
                Engine::Yugabyte => {
                    t!("insight.non_sargable.fix.yugabyte", column = &col, table = &leaf, function = &lower)
                }
                Engine::Postgres => {
                    t!("insight.non_sargable.fix.postgres", column = &col, table = &leaf, function = &lower)
                }
                Engine::Mysql if is_mariadb(cx.plan) => {
                    t!("insight.non_sargable.fix.mariadb", column = &col, function = &func)
                }
                Engine::Mysql => t!("insight.non_sargable.fix.mysql", column = &col, table = &leaf, function = &lower),
                Engine::Mssql => t!("insight.non_sargable.fix.mssql", column = &col, function = &func),
                Engine::Sqlite => {
                    t!("insight.non_sargable.fix.sqlite", column = &col, table = &leaf, function = &lower)
                }
            }
        };
        d.verify = t!("insight.non_sargable.verify", table = &leaf, column = &col);
        out.push(d);
    }
    out
}

pub(super) fn leading_wildcard(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| index_could_pay(cx, n) || seek_filter_could_pay(cx, n)) {
        let Some(filter) = scan_filter_text(cx, n) else { continue };
        let Some(Blocker::Wildcard(at)) = blocker(cx, n, &filter) else { continue };
        let column = identifiers(&filter[..at]).into_iter().last();
        let (named, col) = (known(column.is_some()), column.clone().unwrap_or_default());
        let table = what(n);
        let leaf = table_leaf(&table);
        let impact = cx.share(n).max(cx.subtree(n) * 0.5);
        let mut d = Draft::new("leading-wildcard", InsightArea::Query, impact, vec![n.id]);
        d.title = t!("insight.leading_wildcard.title", named = named, column = &col, table = &leaf);
        let read = rows_read(n);
        d.detail = t!(
            "insight.leading_wildcard.detail",
            op = &n.op,
            named = named,
            column = &col,
            n = read,
            rows = format::count(read),
            kept = format::percent(filter_keeps(cx, n).unwrap_or(1.0))
        );
        d.fix = match cx.engine() {
            Engine::Yugabyte => t!("insight.leading_wildcard.fix.yugabyte"),
            Engine::Postgres => t!("insight.leading_wildcard.fix.postgres", named = named, column = &col),
            Engine::Mssql => t!("insight.leading_wildcard.fix.mssql", named = named, column = &col),
            Engine::Mysql => t!("insight.leading_wildcard.fix.mysql"),
            Engine::Sqlite => t!("insight.leading_wildcard.fix.sqlite"),
        };
        let facts = n.facts();
        let kind = facts.table_kind.as_deref();
        // An index needs the column the pattern is on: none to create where the filter doesn't name it.
        let trigram = cx.engine() == Engine::Postgres && kind != Some("timescale-unresolved");
        d.fix_sql = column.filter(|_| trigram).map(|column| {
            let concurrently = if matches!(kind, Some("hypertable" | "timescale-chunk")) { "" } else { " CONCURRENTLY" };
            format!(
                "CREATE EXTENSION IF NOT EXISTS pg_trgm;\nCREATE INDEX{concurrently} {}\n    ON {} USING gin ({} gin_trgm_ops);",
                quote(Engine::Postgres, &format!("ix_{}_{}_trgm", leaf.to_ascii_lowercase(), column.to_ascii_lowercase())),
                qualified(Engine::Postgres, &table),
                quote(Engine::Postgres, &column)
            )
        });
        d.verify = t!("insight.leading_wildcard.verify", table = &leaf);
        out.push(d);
    }
    out
}

/// Joins that combine every row with every row: a NoJoinPredicate warning, or a Postgres/MySQL join with no condition.
pub(super) fn cross_joins(cx: &Cx) -> Vec<u32> {
    let mut ids: Vec<u32> =
        cx.nodes.iter().filter(|n| n.warnings.contains(&PlanWarning::NoJoinPredicate)).map(|n| n.id).collect();
    if ids.is_empty() && cx.plan.warnings.contains(&PlanWarning::NoJoinPredicate) {
        ids.extend(cx.nodes.iter().filter(|n| n.kind == NodeKind::Join).take(1).map(|n| n.id));
    }
    if cx.engine().sql_syntax() == crate::SqlSyntax::Postgres {
        for n in cx.nodes.iter().filter(|n| n.op == "Nested Loop" && n.children.len() == 2) {
            let inner = &n.children[1];
            // Anywhere under the inner side: a LATERAL subquery's condition sits below its Limit and Sort
            // (Limit → Sort → Index Scan with the Index Cond).
            fn conditioned(n: &PlanNode) -> bool {
                !conditions(n).is_empty() || n.facts().join_filter.is_some() || n.children.iter().any(conditioned)
            }
            let no_condition = !conditioned(inner) && !n.facts().join_filter.is_some();
            let product = n.children[0].est_rows * inner.est_rows.max(1.0);
            if no_condition && n.est_rows >= 0.5 * product && product >= 100.0 {
                ids.push(n.id);
            }
        }
    }
    if cx.engine() == Engine::Mysql {
        ids.extend(
            cx.nodes
                .iter()
                .filter(|n| {
                    (n.facts().join_buffer.is_some() && n.predicate.is_none() && n.seek_predicate.is_none())
                        || n.op.to_ascii_lowercase().contains("(no condition)")
                })
                .map(|n| n.id),
        );
    }
    ids
}

/// A NOT IN that checks the subquery row by row, and the operators that work exists for (what NOT EXISTS removes).
/// NOT IN that is one build and a probe per row isn't one: PostgreSQL's hashed SubPlan, MySQL's materialized subquery
/// and its antijoin (`Not exists`, MySQL 8.0.17+).
pub(super) fn not_in_work<'a>(cx: &Cx<'a>, n: &'a PlanNode) -> Option<Vec<&'a PlanNode>> {
    let statement = cx.statement();
    // The same plan shapes come from NOT EXISTS under an OR; when the statement is known, it must say NOT IN.
    let says_not_in = statement.is_empty() || statement.contains("not in");
    match cx.engine() {
        // EXPLAIN QUERY PLAN shows no per-row work to weigh.
        Engine::Sqlite => None,
        Engine::Mssql => {
            let spools: Vec<&PlanNode> = n
                .children
                .iter()
                .flat_map(|c| std::iter::once(c).chain(c.children.iter()))
                .filter(|c| c.op == "Row Count Spool")
                .collect();
            // NOT EXISTS gets the same spool under an anti semi join (a correlated condition the inner side can't use).
            (n.detail.as_deref().is_some_and(|d| d.contains("Anti Semi Join")) && !spools.is_empty() && says_not_in)
                .then_some(spools)
        }
        Engine::Postgres | Engine::Yugabyte => {
            let l = n.predicate.as_deref()?.to_ascii_lowercase();
            let per_row = l.contains("not (") && l.contains("subplan") && !l.contains("hashed subplan");
            let subplans: Vec<&PlanNode> = n
                .children
                .iter()
                .filter(|c| {
                    matches!(c.facts().parent_relationship.as_ref(), Some(crate::PlanParentRelationship::SubPlan))
                })
                .collect();
            (per_row && says_not_in).then_some(subplans)
        }
        Engine::Mysql => {
            let l = n.predicate.as_deref()?.to_ascii_lowercase();
            let negated = l.contains("<not>(<in_optimizer>") || l.contains("not(<in_optimizer>");
            // `<exists>` is the subquery run for each row; `<materialize>` is built once and looked up.
            let per_row = negated && l.contains("<exists>") && !l.contains("<materialize>");
            (per_row && says_not_in && n.facts().anti_join != Some(true)).then(|| n.children.iter().collect())
        }
    }
}

pub(super) fn not_in(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in &cx.nodes {
        let Some(work) = not_in_work(cx, n) else { continue };
        // NOT IN's own work: the operator testing each row and the NULL handling or subquery it repeats, not the
        // input it filters, which NOT EXISTS reads all the same.
        let impact = (cx.share(n) + work.iter().map(|w| cx.subtree(w)).sum::<f64>()).min(1.0);
        let mut nodes = vec![n.id];
        nodes.extend(work.iter().map(|w| w.id));
        let mut d = Draft::new("not-in", InsightArea::Query, impact, nodes);
        d.title = match cx.engine() {
            Engine::Mssql => t!("insight.not_in.title.mssql"),
            Engine::Postgres | Engine::Yugabyte => t!("insight.not_in.title.postgres"),
            Engine::Sqlite => t!("insight.not_in.title.sqlite"),
            Engine::Mysql => t!("insight.not_in.title.mysql"),
        };
        d.detail = match cx.engine() {
            Engine::Mssql => t!("insight.not_in.detail.mssql", op = &n.op),
            Engine::Postgres | Engine::Yugabyte => t!("insight.not_in.detail.postgres"),
            Engine::Sqlite => t!("insight.not_in.detail.sqlite"),
            Engine::Mysql => t!("insight.not_in.detail.mysql"),
        };
        d.fix = t!("insight.not_in.fix");
        d.verify = t!("insight.not_in.verify");
        out.push(d);
    }
    out
}

pub(super) fn scalar_udf(cx: &Cx) -> Vec<Draft> {
    if cx.engine() != Engine::Mssql {
        return Vec::new();
    }
    let blocks_parallel =
        cx.plan.facts().parallelism_blocker.as_deref() == Some("TSQLUserDefinedFunctionsNotParallelizable");
    let mut out = Vec::new();
    // Time spent in scalar functions, measured apart from the plan's costs (SQL Server 2016 SP2+ actual plans).
    let measured = match (cx.plan.facts().udf_elapsed_ms, elapsed_ms(cx.plan)) {
        (Some(udf), Some(total)) if total > 0.0 => Some((udf, (udf / total).min(1.0))),
        _ => None,
    };
    for n in cx.nodes.iter().filter(|n| !n.facts().user_functions.is_empty()) {
        let names = n.facts().user_functions.join(", ");
        // In a seek's keys a function is worked out once per seek, not per row.
        let in_seek = n.facts().functions_in_seek_only == Some(true);
        let calls = if in_seek { n.runs() } else { rows_out(n).max(n.children.first().map_or(0.0, rows_out)) };
        // The plan's costs leave the function's body out, so its own share says little: the measured time does, and
        // without it the number of calls is the evidence.
        let impact = measured.map_or_else(|| cx.share(n), |(_, share)| share);
        let mut d = Draft::new("scalar-udf", InsightArea::Query, impact, vec![n.id]);
        if measured.is_none() && calls >= MANY_ROWS {
            d.floor = Severity::Medium;
        }
        d.title = if in_seek {
            t!("insight.scalar_udf.title.per_seek", names = &names, n = calls, calls = format::count(calls))
        } else {
            t!("insight.scalar_udf.title.per_row", names = &names, n = calls, calls = format::count(calls))
        };
        d.detail = [
            if blocks_parallel {
                t!("insight.scalar_udf.detail.per_row_serial")
            } else {
                t!("insight.scalar_udf.detail.per_row")
            },
            match measured {
                Some((ms, share)) => t!(
                    "insight.scalar_udf.detail.measured",
                    time = format::duration(ms),
                    share = format::percent(share)
                ),
                None => t!("insight.scalar_udf.detail.unmeasured"),
            },
        ]
        .join(" ");
        d.fix = t!("insight.scalar_udf.fix");
        d.verify = t!("insight.scalar_udf.verify");
        out.push(d);
    }
    if out.is_empty() && blocks_parallel {
        let mut d = Draft::new("scalar-udf", InsightArea::Query, 0.1, vec![cx.plan.root.id]);
        d.fixed_weight = true;
        d.subtree = true;
        d.floor = Severity::Medium;
        d.title = t!("insight.scalar_udf.serial.title");
        d.detail = t!("insight.scalar_udf.serial.detail");
        d.fix = t!("insight.scalar_udf.serial.fix");
        d.verify = t!("insight.scalar_udf.serial.verify");
        out.push(d);
    }
    out
}

pub(super) fn nested_loops(cx: &Cx) -> Vec<Draft> {
    let cross = cross_joins(cx);
    let not_in_ids: Vec<u32> = not_in(cx).iter().flat_map(|d| d.nodes.clone()).collect();
    let mut out = Vec::new();
    for n in
        cx.nodes.iter().filter(|n| n.kind == NodeKind::Join && n.op.to_ascii_lowercase().starts_with("nested loop"))
    {
        if n.children.len() < 2 {
            continue;
        }
        let outer = &n.children[0];
        let inner = &n.children[n.children.len() - 1];
        // How often an estimated loop repeats its inner side, where its outer side was planned on a default guess for a
        // parameter, is that guess, not the data.
        if inner.act_executions.is_none() && cx.guessed.contains(&inner.id) {
            continue;
        }
        // Said better elsewhere: key lookups, NOT IN's row count spool, a cross join. A join buffer, MySQL's or
        // MariaDB's (`block-nl-join`), is no repeat: the table is read once per buffer fill, not once per row.
        let mut inner_nodes = Vec::new();
        inner.walk(&mut inner_nodes);
        let buffered = |x: &PlanNode| x.facts().join_buffer.is_some() || x.facts().join_buffer_join_type.is_some();
        if inner.kind == NodeKind::Lookup
            || inner_nodes.iter().any(|x| x.op == "Row Count Spool" || buffered(x))
            || cross.contains(&n.id)
            || inner_nodes.iter().any(|x| not_in_ids.contains(&x.id))
        {
            continue;
        }
        // An eager index spool is the eager-spool rule's: it is built once per run, not repeated per row.
        if inner_nodes.iter().any(|x| x.op == "Index Spool" && x.detail.as_deref() == Some("Eager Spool")) {
            continue;
        }
        // Times the inner side is asked for rows: once per outer row.
        let asked =
            if inner.act_executions.is_some() || inner.est_executions > 1.0 { inner.runs() } else { rows_out(outer) };
        // A cache in front of the inner side (PostgreSQL's Memoize and Materialize, SQL Server's lazy spools, MySQL's
        // materialized subqueries) replays rows it holds: its input runs again only on a cache miss or a rebind, which
        // is how often that input ran.
        let materializes = |x: &PlanNode| {
            matches!(x.op.as_str(), "Memoize" | "Table Spool" | "Index Spool" | "Query block" | "Materialized subquery")
                || x.op.starts_with("Materialize")
        };
        let cache_node = if materializes(inner) {
            Some(inner)
        } else if inner.object.is_none() {
            inner.children.iter().find(|c| materializes(c))
        } else {
            None
        };
        let cached_input = cache_node.and_then(|c| c.children.first());
        let cache = cache_node.is_some();
        let reads = if cache { cached_input.map_or(0.0, |c| c.runs()) } else { asked };
        let outer_name = table_leaf(&first_object(outer).unwrap_or_else(|| outer.op.clone()));
        let inner_name = table_leaf(&first_object(inner).unwrap_or_else(|| inner.op.clone()));
        let hinted = {
            let s = cx.statement();
            s.contains("loop join") || s.contains("option (loop")
        };
        // The optimizer's choice stands unless the plan shows it was made on wrong numbers, or is forced, or each run
        // rereads much of a table: an accurate outer estimate means a loop was costed against a hash join and won.
        let expected = outer.est_rows_for_runs();
        let misjudged = outer.act_rows.is_some_and(|a| a >= MISESTIMATE_FACTOR * expected.max(1.0));
        let leaf = inner_nodes.iter().find(|x| x.object.is_some() && x.kind != NodeKind::Other).copied();
        let per_read = leaf.map(|l| rows_read(l) / l.runs().max(1.0));
        let table = leaf.and_then(|l| l.table_rows).filter(|t| *t > 0.0);
        let rereads = per_read.is_some_and(|per| {
            per >= 100.0
                && match table {
                    Some(t) => per >= t * INDEX_KEEPS_AT_MOST,
                    None => leaf.is_some_and(|l| l.kind == NodeKind::Scan),
                }
        });
        // Each outer row meets every row the cache replays, and the join tests its condition on every pair.
        let per_replay = rows_out(inner) / asked.max(1.0);
        let pairs = cache && n.predicate.is_some() && asked >= MANY_LOOPS && per_replay >= 100.0;
        let repeated = reads >= MANY_LOOPS && (hinted || misjudged || rereads);
        if !repeated && !pairs {
            continue;
        }
        let impact = (cx.subtree(inner) + cx.share(n)).min(1.0);
        let mut d = Draft::new("nested-loops", InsightArea::Query, impact, vec![n.id, inner.id]);
        d.cap = Severity::Medium;
        let mysql = cx.engine() == Engine::Mysql;
        let inner_op = &inner.op;
        d.title = if repeated {
            t!(
                "insight.nested_loops.title.repeats",
                inner = &inner_name,
                n = reads,
                times = format::count(reads),
                outer = &outer_name
            )
        } else {
            t!(
                "insight.nested_loops.title.pairs",
                n = asked,
                rows = format::count(asked),
                outer = &outer_name,
                replayed = format::count(per_replay),
                op = inner_op,
                pairs = format::count(asked * per_replay)
            )
        };
        let work = cx.work_sentence(impact, &[n.id, inner.id]);
        d.detail = if hinted && repeated {
            t!("insight.nested_loops.forced")
        } else if misjudged && repeated {
            [
                t!(
                    "insight.nested_loops.detail.misjudged",
                    n = expected,
                    expected = format::count(expected),
                    outer = &outer_name,
                    got = rows_text(rows_out(outer))
                ),
                if mysql {
                    t!("insight.nested_loops.detail.misjudged_mysql")
                } else {
                    t!("insight.nested_loops.detail.misjudged_hash")
                },
            ]
            .join(" ")
        } else if repeated {
            let per_read = per_read.unwrap_or(0.0);
            let reread = match table {
                Some(t) => t!(
                    "insight.nested_loops.detail.rereads_share",
                    n = per_read,
                    rows = format::count(per_read),
                    inner = &inner_name,
                    share = format::percent((per_read / t).min(1.0)),
                    times = format::count(reads)
                ),
                None => t!(
                    "insight.nested_loops.detail.rereads",
                    n = per_read,
                    rows = format::count(per_read),
                    inner = &inner_name,
                    times = format::count(reads)
                ),
            };
            std::iter::once(reread).chain(work).collect::<Vec<_>>().join(" ")
        } else {
            let replays = t!(
                "insight.nested_loops.detail.replays",
                op = inner_op,
                inner = &inner_name,
                n = per_replay,
                rows = format::count(per_replay),
                outer = &outer_name,
                predicate = format::predicate(n.predicate.as_deref().unwrap_or_default())
            );
            let forced = hinted.then(|| t!("insight.nested_loops.forced"));
            std::iter::once(replays).chain(work).chain(forced).collect::<Vec<_>>().join(" ")
        };
        d.fix = if hinted {
            t!("insight.nested_loops.fix.hint")
        } else if misjudged && repeated {
            if mysql {
                t!("insight.nested_loops.fix.estimate_mysql", outer = &outer_name)
            } else {
                t!("insight.nested_loops.fix.estimate", outer = &outer_name)
            }
        } else if repeated && leaf.is_some_and(|l| l.kind == NodeKind::Seek) {
            // It already seeks an index, one that narrows each run too little.
            t!("insight.nested_loops.fix.narrower_index", inner = &inner_name, outer = &outer_name)
        } else if repeated {
            t!("insight.nested_loops.fix.index", inner = &inner_name, outer = &outer_name)
        } else {
            t!("insight.nested_loops.fix.condition", outer = &outer_name)
        };
        d.verify = if mysql {
            t!("insight.nested_loops.verify.mysql", inner = &inner_name)
        } else if hinted {
            t!("insight.nested_loops.verify.hint")
        } else if repeated {
            t!("insight.nested_loops.verify.repeats", inner = &inner_name)
        } else {
            t!("insight.nested_loops.verify.pairs", op = inner_op)
        };
        out.push(d);
    }
    out
}

pub(super) fn no_join_predicate(cx: &Cx) -> Vec<Draft> {
    let ids = cross_joins(cx);
    cx.nodes
        .iter()
        .filter(|n| ids.contains(&n.id))
        .copied()
        .collect::<Vec<&PlanNode>>()
        .into_iter()
        // A side of one row (a total, a setting) multiplies nothing: joining to it without a condition is harmless.
        .filter(|n| {
            // Under a TOP, SQL Server estimates each side for the rows it expects to need; the side itself is bigger.
            let per_run = |c: &PlanNode| {
                (rows_out(c) / c.runs().max(1.0)).max(c.facts().estimated_rows_without_goal.unwrap_or(0.0))
            };
            let sides: Vec<f64> = if n.children.len() >= 2 {
                n.children.iter().map(per_run).collect()
            } else {
                // MySQL's table joined through a join buffer: the table, and the rows joined before it.
                let before = cx.parent(n).and_then(|p| {
                    let at = p.children.iter().position(|c| c.id == n.id)?;
                    (at > 0).then(|| rows_out(&p.children[at - 1]))
                });
                vec![rows_read(n) / n.runs().max(1.0), before.unwrap_or(0.0)]
            };
            sides.iter().all(|rows| *rows >= 2.0)
        })
        .take(1)
        .map(|n| {
            let sides: Vec<String> = n.children.iter().filter_map(first_object).map(|o| table_leaf(&o)).collect();
            let impact = cx.subtree(n);
            let mut d = Draft::new("no-join-predicate", InsightArea::Query, impact, vec![n.id]);
            d.title = if sides.len() >= 2 {
                t!("insight.no_join_predicate.title_sides", first = &sides[0], second = &sides[1])
            } else {
                t!("insight.no_join_predicate.title")
            };
            let rows = rows_out(n);
            d.detail = t!("insight.no_join_predicate.detail", op = &n.op, n = rows, rows = format::count(rows));
            d.fix = t!("insight.no_join_predicate.fix");
            d.verify = t!("insight.no_join_predicate.verify");
            d
        })
        .collect()
}

pub(super) fn optimizer_limits(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    let facts = cx.plan.facts();
    if let Some(reason) =
        facts.optimization_early_abort.as_deref().filter(|r| matches!(*r, "TimeOut" | "MemoryLimitExceeded"))
    {
        let mut d = Draft::new("optimizer-timeout", InsightArea::Query, 0.1, vec![cx.plan.root.id]);
        // The optimizer says it stopped searching, not that the plan it kept is a bad one: Low on its word alone.
        d.fixed_weight = true;
        d.subtree = true;
        d.cap = Severity::Medium;
        let memory = reason.eq_ignore_ascii_case("MemoryLimitExceeded");
        d.title =
            if memory { t!("insight.optimizer_timeout.title_memory") } else { t!("insight.optimizer_timeout.title") };
        d.detail = t!("insight.optimizer_timeout.detail", reason = reason);
        d.fix = t!("insight.optimizer_timeout.fix");
        d.verify = t!("insight.optimizer_timeout.verify");
        out.push(d);
    }
    if cx.plan.facts().cardinality_estimator == Some(70) {
        let mut d = Draft::new("legacy-ce", InsightArea::Server, 0.02, vec![cx.plan.root.id]);
        d.fixed_weight = true;
        d.subtree = true;
        d.cap = Severity::Low;
        d.title = t!("insight.legacy_ce.title");
        d.detail = t!("insight.legacy_ce.detail");
        d.fix = t!("insight.legacy_ce.fix");
        d.verify = t!("insight.legacy_ce.verify");
        out.push(d);
    }
    out
}

pub(super) fn slow_compile(cx: &Cx) -> Vec<Draft> {
    let (planning, running) = match cx.engine() {
        Engine::Mssql => (cx.plan.facts().compile_ms, None),
        Engine::Postgres | Engine::Yugabyte => (cx.plan.facts().planning_ms, cx.plan.facts().elapsed_ms),
        Engine::Mysql | Engine::Sqlite => (None, None),
    };
    let Some(planning) = planning else { return Vec::new() };
    let slower_than_running = running.is_some_and(|r| planning > r && planning >= 20.0);
    if planning < 500.0 && !slower_than_running {
        return Vec::new();
    }
    let running = running.or_else(|| elapsed_ms(cx.plan));
    let impact = running.map_or(0.1, |r| planning / (planning + r).max(1.0));
    let mut d = Draft::new("slow-compile", InsightArea::Query, impact, vec![cx.plan.root.id]);
    // A cached plan records the one compile that produced it: without a run to weigh it against, nothing says it
    // recompiles, and it is Low.
    d.fixed_weight = running.is_none();
    d.cost_ms = Some(planning);
    d.cap = Severity::Medium;
    // PostgreSQL's first planning on a connection also loads the tables' metadata into the backend's caches, and
    // Culprit's EXPLAIN runs on a fresh connection: a few tens of milliseconds may never recur.
    let cold = matches!(cx.engine(), Engine::Postgres | Engine::Yugabyte);
    if cold && planning < 500.0 {
        d.cap = Severity::Low;
    }
    d.title = match running {
        Some(r) if slower_than_running => t!(
            "insight.slow_compile.title_slower",
            planning = format::duration(planning),
            running = format::duration(r)
        ),
        _ => t!("insight.slow_compile.title", planning = format::duration(planning)),
    };
    let mut detail = vec![t!("insight.slow_compile.detail")];
    if cold {
        detail.push(t!("insight.slow_compile.detail_cold"));
    }
    d.detail = detail.join(" ");
    d.fix = match cx.engine() {
        Engine::Postgres | Engine::Yugabyte => t!("insight.slow_compile.fix_prepared"),
        _ => t!("insight.slow_compile.fix"),
    };
    d.verify = t!("insight.slow_compile.verify");
    vec![d]
}

pub(super) fn remote_query(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        .filter(|n| n.op.starts_with("Remote"))
        .take(1)
        .map(|n| {
            let mut d = Draft::new("remote-query", InsightArea::Query, cx.subtree(n), vec![n.id]);
            d.cap = Severity::Medium;
            // Remote Insert, Update and Delete send rows the other way: each one is a round trip to the other server.
            let writes = ["Remote Insert", "Remote Update", "Remote Delete"].contains(&n.op.as_str());
            if writes {
                let rows = n.children.first().map_or_else(|| rows_out(n), rows_out);
                d.title =
                    t!("insight.remote_query.writes.title", n = plural_n(rows), count = rows_text(rows), op = &n.op);
                d.detail = t!("insight.remote_query.writes.detail");
                d.fix = t!("insight.remote_query.writes.fix");
                d.verify = t!("insight.remote_query.writes.verify");
            } else {
                let rows = rows_out(n);
                d.title = t!("insight.remote_query.reads.title", n = plural_n(rows), count = rows_text(rows));
                d.detail = t!("insight.remote_query.reads.detail");
                d.fix = t!("insight.remote_query.reads.fix");
                d.verify = t!("insight.remote_query.reads.verify");
            }
            d
        })
        .collect()
}

pub(super) fn correlated_subquery(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in &cx.nodes {
        let Some(parent) = cx.parent(n) else { continue };
        // NOT IN explains its own subquery and has the better fix.
        if not_in_work(cx, parent).is_some() {
            continue;
        }
        // Without measured runs, the parent's rows bound them from above: "up to" that many.
        let mut at_most = false;
        let (runs, outer, impact) = match cx.engine() {
            Engine::Postgres | Engine::Yugabyte => {
                if !matches!(n.facts().parent_relationship.as_ref(), Some(crate::PlanParentRelationship::SubPlan)) {
                    continue;
                }
                // A hashed SubPlan runs once and is probed per row: `(hashed SubPlan 1)` in the parent's condition.
                let name = n.facts().subplan_name.as_ref().map_or("subplan".to_string(), |s| s.to_ascii_lowercase());
                let parent_text = parent
                    .facts()
                    .conditions
                    .iter()
                    .chain(parent.predicate.iter())
                    .chain(parent.seek_predicate.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                if parent_text.to_ascii_lowercase().contains(&format!("hashed {name}")) {
                    continue;
                }
                // Measured runs. Else, in one of the parent's conditions, it runs for each row the parent tests, at most
                // the rows it reads (another condition may reject a row first); anywhere else (the select list), once
                // for each row the parent returns, never for each row of a table the parent scans to return a few.
                let runs = match n.act_executions {
                    Some(runs) => runs,
                    None if names_subplan(parent, &name) => {
                        at_most = true;
                        rows_read(parent)
                    }
                    None => rows_out(parent),
                };
                // PostgreSQL charges every run of the SubPlan to the parent's own cost.
                (runs, first_object(parent), (cx.share(parent) + cx.subtree(n)).min(1.0))
            }
            Engine::Mysql => {
                if n.facts().dependent != Some(true) || n.facts().cacheable == Some(true) {
                    continue;
                }
                let runs = if n.facts().expression_cache {
                    // MariaDB's expression cache answers repeated outer values: only measured misses are runs.
                    let Some(runs) = n.act_executions else { continue };
                    runs
                } else if parent.object.is_some() {
                    // Attached to a table's condition: run for each row the table reads.
                    rows_read(parent)
                } else {
                    // In the select list: run for each row the query block returns.
                    parent.est_rows
                };
                let outer = if parent.object.is_some() {
                    parent.object.clone()
                } else {
                    parent
                        .children
                        .iter()
                        .find(|c| !matches!(c.op.as_str(), "Subquery" | "Dependent subquery"))
                        .and_then(first_object)
                };
                // MySQL costs one run of the subquery; the plan's row count says how many there are.
                let one = n.subtree_cost.max(0.0);
                let all = one * runs.max(1.0);
                let impact = if all > 0.0 { all / (cx.total - one + all) } else { 0.0 };
                (runs, outer, impact.clamp(0.0, 1.0))
            }
            Engine::Mssql | Engine::Sqlite => continue,
        };
        if runs < MANY_RUNS {
            continue;
        }
        let mut d = Draft::new("correlated-subquery", InsightArea::Query, impact, vec![n.id]);
        let (named, outer_name) = (known(outer.is_some()), outer.as_deref().map(table_leaf).unwrap_or_default());
        let count = format::count(runs);
        d.title = if at_most {
            t!("insight.correlated_subquery.title_at_most", n = runs, count = count, named = named, outer = &outer_name)
        } else {
            t!("insight.correlated_subquery.title", n = runs, count = count, named = named, outer = &outer_name)
        };
        let mysql = cx.engine() == Engine::Mysql;
        d.detail = match first_object(n).map(|t| table_leaf(&t)) {
            Some(table) if mysql => t!("insight.correlated_subquery.detail.mysql_reading", table = table),
            Some(table) => t!("insight.correlated_subquery.detail.postgres_reading", table = table),
            None if mysql => t!("insight.correlated_subquery.detail.mysql"),
            None => t!("insight.correlated_subquery.detail.postgres"),
        };
        d.fix = t!("insight.correlated_subquery.fix");
        d.verify = t!("insight.correlated_subquery.verify");
        out.push(d);
    }
    out.into_iter().take(2).collect()
}

/// Whether one of a PostgreSQL operator's conditions runs the SubPlan `name` ("subplan 1", lowercase): `(total >
/// (SubPlan 1))` in its Filter, never `SubPlan 12`.
fn names_subplan(n: &PlanNode, name: &str) -> bool {
    let facts = n.facts();
    [n.predicate.as_deref(), n.seek_predicate.as_deref()]
        .into_iter()
        .flatten()
        .chain(facts.conditions.iter().map(String::as_str))
        .any(|text| {
            let text = text.to_ascii_lowercase();
            text.match_indices(name).any(|(at, _)| !text[at + name.len()..].starts_with(|c: char| c.is_ascii_digit()))
        })
}

pub(super) fn temp_table_sort(cx: &Cx) -> Vec<Draft> {
    let server = if cx.mariadb() { "MariaDB" } else { "MySQL" };
    let in_temp =
        |n: &PlanNode| n.op == "Temporary table" || n.detail.as_deref().is_some_and(|d| d.contains("temporary table"));
    // MySQL marks the grouping step (`grouping_operation`, op Group) or the ordering step (`ordering_operation`, op
    // Sort); MariaDB wraps a "Temporary table" node, sorted when a Sort wraps that.
    let grouping = |n: &PlanNode| matches!(n.op.as_str(), "Group" | "Distinct" | "Temporary table");
    let sorted = |n: &PlanNode| {
        n.detail.as_deref().is_some_and(|d| d.contains("filesort"))
            || cx.nodes.iter().any(|p| p.kind == NodeKind::Sort && p.children.iter().any(|c| c.id == n.id))
    };
    fn grouped_below(n: &PlanNode, in_temp: &dyn Fn(&PlanNode) -> bool) -> bool {
        n.children.iter().any(|c| {
            (matches!(c.op.as_str(), "Group" | "Distinct" | "Temporary table") && in_temp(c))
                || grouped_below(c, in_temp)
        })
    }
    // A temporary table for an ORDER BY alone (on a table the join doesn't read first) has no grouping to explain.
    let statement = cx.statement();
    let groups = statement.is_empty() || statement.contains("group by") || statement.contains("distinct");
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| in_temp(n)) {
        let rows = n.children.iter().map(rows_out).fold(0.0, f64::max).max(rows_out(n));
        if rows < MANY_ROWS {
            continue;
        }
        if !groups {
            let sort_above = cx.parent(n).filter(|p| p.kind == NodeKind::Sort).map_or(0.0, |p| cx.share(p));
            let mut d =
                Draft::new("temp-table-sort", InsightArea::Query, (cx.share(n) + sort_above).min(1.0), vec![n.id]);
            d.cap = Severity::Low;
            d.title = t!("insight.temp_table_sort.order.title", n = rows, count = format::count(rows));
            d.detail = t!("insight.temp_table_sort.order.detail", server = server);
            d.fix = t!("insight.temp_table_sort.order.fix");
            d.verify = t!("insight.temp_table_sort.order.verify");
            out.push(d);
            continue;
        }
        // The temporary table's own work and the sort of it, not the reads below it, which happen either way. MySQL
        // leaves most of it uncosted, so this is often small next to the scan feeding it.
        let sort_above = cx.parent(n).filter(|p| p.kind == NodeKind::Sort).map_or(0.0, |p| cx.share(p));
        let impact = (cx.share(n) + sort_above).min(1.0);
        if grouping(n) {
            let mut d = Draft::new("temp-table-sort", InsightArea::Index, impact, vec![n.id]);
            d.title = if sorted(n) {
                t!("insight.temp_table_sort.group.title_sorted", n = rows, count = format::count(rows))
            } else {
                t!("insight.temp_table_sort.group.title", n = rows, count = format::count(rows))
            };
            d.detail = t!("insight.temp_table_sort.group.detail", server = server);
            d.fix = t!("insight.temp_table_sort.group.fix", server = server);
            d.verify = t!("insight.temp_table_sort.group.verify");
            out.push(d);
        } else if !grouped_below(n, &in_temp) {
            // The ordering step's temporary table: the grouping below already reads an index in order, so only the
            // groups are sorted. No index changes that when the order is by an aggregate.
            let mut d = Draft::new("temp-table-sort", InsightArea::Query, impact, vec![n.id]);
            d.cap = Severity::Low;
            d.title = t!("insight.temp_table_sort.groups.title");
            d.detail = t!("insight.temp_table_sort.groups.detail", server = server);
            d.fix = t!("insight.temp_table_sort.groups.fix");
            d.verify = t!("insight.temp_table_sort.groups.verify");
            out.push(d);
        }
    }
    out
}
