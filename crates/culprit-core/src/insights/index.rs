//! Plan insights about indexes: lookups back to the table, scans that filter, the optimizer's index requests and
//! the indexes an engine builds for itself.

use super::*;

/// A `CREATE INDEX` to copy, in the engine's dialect. A plan doesn't say the server's edition, so SQL Server's online
/// build is suggested, not assumed. Culprit never runs it.
pub(super) fn index_sql(
    engine: Engine,
    table: &str,
    keys: &[String],
    include: &[String],
    kind: Option<&str>,
) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    // Named as the findings name the same index (`support::index_name` in culprit-checks), so a plan's insight and the
    // finding about it never spell one index two ways: `IX_`, the table and the keys, lower case where the engine folds
    // names.
    let name: String = std::iter::once(format!("IX_{}", table_leaf(table)))
        .chain(keys.iter().map(|k| k.replace(' ', "")))
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .take(60)
        .collect();
    let name = if engine == Engine::Mssql { name } else { name.to_ascii_lowercase() };
    let target = qualified(engine, table);
    Some(engine.spec().table_dialect(kind).create_index(&crate::dialect::NewIndex {
        target: &target,
        unique: false,
        name: &name,
        keys,
        include,
        filter: None,
        online: crate::dialect::Online::Unknown,
    }))
}

/// Output columns of an operator (`OutputList` on SQL Server), as plain names.
pub(super) fn output_columns(n: &PlanNode) -> Vec<String> {
    n.facts()
        .output_columns
        .as_ref()
        .map(|list| list.iter().filter(|c| !c.is_empty() && !c.starts_with("Expr")).cloned().collect())
        .unwrap_or_default()
}

/// Rows a lookup step fetches one at a time: its executions, or the rows it returns when that's more.
fn lookups(n: &PlanNode) -> f64 {
    n.act_executions.unwrap_or(n.est_executions).max(rows_out(n))
}

/// Whether a key or RID lookup fetches enough rows one at a time to be worth covering: the `key-lookups` rule's bar.
fn many_lookups(n: &PlanNode) -> bool {
    n.kind == NodeKind::Lookup && n.op.contains("Lookup") && lookups(n) >= MANY_RUNS
}

pub(super) fn key_lookups(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| many_lookups(n)) {
        let runs = lookups(n);
        let table = what(n);
        let cols = output_columns(n);
        let seek = cx.parent(n).and_then(|p| p.children.first()).filter(|s| s.id != n.id && s.index.is_some());
        let index = seek.and_then(|s| s.index.clone());
        let impact = cx.share(n) + cx.parent(n).filter(|p| p.kind == NodeKind::Join).map_or(0.0, |p| cx.share(p));
        let mut nodes = vec![n.id];
        nodes.extend(seek.map(|s| s.id));
        // The share only where the plan states one (`stated_share`).
        let together =
            cx.states_share(&nodes).then(|| t!("insight.key_lookups.together", share = format::percent(impact)));
        let mut d = Draft::new("key-lookups", InsightArea::Index, impact, nodes);
        // An estimated plan says how many lookups the optimizer expects, not how many a run made.
        let measured = n.act_executions.is_some() || n.act_rows.is_some();
        let (many, count) = (plural_n(runs), rows_text(runs));
        d.title = if measured {
            t!("insight.key_lookups.title.measured", n = many, count = count, table = &table)
        } else {
            t!("insight.key_lookups.title.estimated", n = many, count = count, table = &table)
        };
        let heap = n.op.starts_with("RID");
        let mut detail = vec![match (&index, cols.is_empty()) {
            (Some(ix), false) => t!(
                "insight.key_lookups.detail.index",
                index = ix,
                columns = format::list(&cols),
                table = &table,
                op = &n.op
            ),
            _ => t!("insight.key_lookups.detail.lookup", table = &table, op = &n.op),
        }];
        detail.extend(together);
        d.detail = detail.join(" ");
        // Said in words only: rebuilding the index needs its whole definition (key order, existing included columns,
        // uniqueness, filter, compression), which the plan doesn't carry.
        let (named, columns) = (known(!cols.is_empty()), format::list(&cols));
        let (indexed, index_name) = (known(index.is_some()), index.clone().unwrap_or_default());
        d.fix = match (&index, heap) {
            (_, true) => [
                t!("insight.key_lookups.fix.heap", table = &table),
                t!(
                    "insight.key_lookups.fix.heap_cover",
                    named = named,
                    columns = &columns,
                    indexed = indexed,
                    index = &index_name
                ),
            ]
            .join(" "),
            (Some(ix), false) => {
                t!("insight.key_lookups.fix.cover_index", named = named, columns = &columns, index = ix)
            }
            (None, false) => t!("insight.key_lookups.fix.cover", named = named, columns = &columns),
        };
        d.verify = t!("insight.key_lookups.verify", op = &n.op, indexed = indexed, index = &index_name);
        out.push(d);
    }
    out
}

pub(super) fn missing_index_hint(cx: &Cx) -> Vec<Draft> {
    // A key lookup on the same table with the same keys already proposes this index, with the columns it needs.
    let lookups: Vec<(String, Vec<String>)> = cx
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Lookup)
        .filter_map(|n| {
            let seek = cx.parent(n)?.children.first().filter(|s| s.id != n.id)?;
            let keys = filter_columns(&conditions(seek)).into_iter().map(|k| k.to_ascii_lowercase()).collect();
            Some((table_leaf(&what(n)).to_ascii_lowercase(), keys))
        })
        .collect();
    let mut hints: Vec<_> = cx
        .plan
        .index_hints
        .iter()
        .filter(|h| {
            let keys: Vec<String> =
                h.equality.iter().chain(&h.inequality).map(|c| clean_identifier(c).to_ascii_lowercase()).collect();
            !lookups.iter().any(|(t, k)| *t == table_leaf(&h.table).to_ascii_lowercase() && *k == keys)
        })
        .collect();
    hints.sort_by(|a, b| b.impact_pct.total_cmp(&a.impact_pct));
    hints
        .into_iter()
        .take(3)
        .map(|h| {
            let leaf = table_leaf(&h.table).to_ascii_lowercase();
            let node = cx
                .nodes
                .iter()
                .filter(|n| n.object.as_deref().is_some_and(|o| table_leaf(o).to_ascii_lowercase() == leaf))
                .max_by(|a, b| a.self_cost.total_cmp(&b.self_cost))
                .copied();
            let keys: Vec<String> = h.equality.iter().chain(&h.inequality).map(|c| clean_identifier(c)).collect();
            let include: Vec<String> = h.include.iter().map(|c| clean_identifier(c)).collect();
            let impact = node.map_or(0.0, |n| cx.share(n)).max(h.impact_pct / 100.0 * 0.5);
            let mut d = Draft::new(
                "missing-index-hint",
                InsightArea::Index,
                impact,
                node.map(|n| vec![n.id]).unwrap_or_default(),
            );
            d.index_hint = Some(h.clone());
            d.title = t!(
                "insight.missing_index_hint.title",
                table = &h.table,
                keys = format::list(&keys),
                share = format::percent(h.impact_pct / 100.0)
            );
            let leaf = table_leaf(&h.table);
            d.detail = match node {
                Some(n) => t!(
                    "insight.missing_index_hint.detail_read",
                    table = &h.table,
                    n = rows_read(n),
                    rows = format::count(rows_read(n)),
                    op = &n.op,
                    leaf = &leaf
                ),
                None => t!("insight.missing_index_hint.detail", table = &h.table, leaf = &leaf),
            };
            d.fix = if include.is_empty() {
                t!("insight.missing_index_hint.fix", keys = format::list(&keys))
            } else {
                t!(
                    "insight.missing_index_hint.fix_include",
                    keys = format::list(&keys),
                    include = format::list(&include)
                )
            };
            d.fix_sql = index_sql(Engine::Mssql, &h.table, &keys, &include, None);
            d.verify = t!("insight.missing_index_hint.verify", table = &leaf);
            d
        })
        .collect()
}

/// The filter a scan applies: its own, or MySQL EXPLAIN ANALYZE's Filter node above it.
pub(super) fn scan_filter_text(cx: &Cx, n: &PlanNode) -> Option<String> {
    n.predicate.clone().or_else(|| cx.parent(n).filter(|p| p.op == "Filter").and_then(|f| f.predicate.clone()))
}

pub(super) fn scan_filter(cx: &Cx) -> Vec<Draft> {
    let hinted: Vec<String> = cx.plan.index_hints.iter().map(|h| table_leaf(&h.table).to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| index_could_pay(cx, n) || seek_filter_could_pay(cx, n)) {
        let Some(table) = actionable_object(n) else { continue };
        let Some(filter) = scan_filter_text(cx, n) else { continue };
        // SQL Server's own suggestion, a conversion, a function or a leading wildcard explain the scan better, and
        // MySQL's ignored index says why there's no seek: their rules say so.
        if hinted.contains(&table_leaf(&table).to_ascii_lowercase())
            || blocker(cx, n, &filter).is_some()
            || (cx.engine() == Engine::Mysql && n.kind == NodeKind::Scan && !possible_keys_for_filter(cx, n).is_empty())
        {
            continue;
        }
        // Only the conditions an index can seek on make the advice; a filter with none of them has no index to give.
        let filter_cols = seekable_columns(&filter);
        if filter_cols.is_empty() {
            continue;
        }
        let (cols, equality) = if let Some(seek) = n.seek_predicate.as_deref().filter(|_| n.kind == NodeKind::Seek) {
            let seek_cols = seekable_columns(seek);
            // A seek whose residual filter checks only columns already in the seek predicate isn't solved by adding
            // another column to the index.
            if seek_cols.is_empty()
                || filter_cols.iter().all(|fc| seek_cols.iter().any(|sc| sc.eq_ignore_ascii_case(fc)))
            {
                continue;
            }
            let combined = format!("{seek} AND {filter}");
            let (eq, _) = seekable_keys(&combined);
            (seekable_columns(&combined), eq)
        } else {
            let (eq, _) = seekable_keys(&filter);
            (filter_cols, eq)
        };
        if cols.is_empty() {
            continue;
        }
        let read = rows_read(n);
        let kept = read * filter_keeps(cx, n).unwrap_or(1.0);
        let impact = cx.share(n).max(cx.subtree(n) * 0.5);
        let mut d = Draft::new("scan-filter", InsightArea::Index, impact, vec![n.id]);
        d.title = t!(
            "insight.scan_filter.title",
            n = plural_n(read),
            count = rows_text(read),
            table = &table,
            kept = rows_text(kept)
        );
        let estimated = n.act_rows.is_none();
        // What the statement's runs measured of the filter, where the analysis put it on the scan.
        let measured = estimated && n.facts().measured_kept_share.is_some();
        let (op, checked) = (&n.op, format::predicate(&filter));
        let mut detail = vec![if measured {
            t!("insight.scan_filter.detail.measured", op = op, filter = checked)
        } else if estimated {
            t!("insight.scan_filter.detail.planned", op = op, filter = checked)
        } else {
            t!("insight.scan_filter.detail.actual", op = op, filter = checked)
        }];
        detail.extend(cx.work_sentence(impact, &[n.id]));
        d.detail = detail.join(" ");
        // What the catalog says the table has, where the snapshot marked it: an index that already starts with one of
        // these columns (or for a seek, covers all of them), or none.
        let leaf = table_leaf(&table);
        let has = table_indexes(n).and_then(|list| {
            if cx.engine() == Engine::Yugabyte && list.iter().any(|i| i.hash_columns.is_none()) {
                return None;
            }
            let mut list = list.into_iter().filter(|i| {
                i.hash_columns.as_ref().is_none_or(|hashes| {
                    hashes.iter().all(|hash| equality.iter().any(|c| c.eq_ignore_ascii_case(hash)))
                })
            });
            if n.kind == NodeKind::Seek {
                Some(
                    list.find(|i| {
                        let keys = &i.columns;
                        keys.len() >= cols.len()
                            && cols.iter().all(|c| keys[..cols.len()].iter().any(|k| k.eq_ignore_ascii_case(c)))
                    })
                    .map(|i| (i.name, i.columns)),
                )
            } else {
                Some(
                    list.find(|i| i.columns.first().is_some_and(|k| cols.iter().any(|c| c.eq_ignore_ascii_case(k))))
                        .map(|i| (i.name, i.columns)),
                )
            }
        });
        // Equality columns go in any order: the filter's isn't the data's. The SQL below takes the filter's order, and
        // the note says so rather than advise an order the SQL doesn't have.
        let order = if equality.len() >= 2 {
            vec![
                t!("insight.scan_filter.order.any", columns = format::list(&equality), first = &equality[0]),
                t!(
                    "insight.scan_filter.order.instead",
                    pair = known(equality.len() == 2),
                    second = &equality[1],
                    table = &leaf
                ),
            ]
        } else {
            Vec::new()
        };
        match has {
            Some(Some((index, keys))) => {
                d.fix = t!("insight.scan_filter.fix.has_index", index = &index, key = &keys[0], table = &leaf);
                d.fix_sql = Some(cx.engine().dialect().refresh_statistics(&qualified(cx.engine(), &table)));
            }
            Some(None) => {
                let starts_with = if n.kind == NodeKind::Seek { format::list(&cols) } else { format::list_or(&cols) };
                let fix = t!(
                    "insight.scan_filter.fix.no_index",
                    table = &leaf,
                    starts_with = starts_with,
                    columns = format::list(&cols)
                );
                d.fix = std::iter::once(fix).chain(order).collect::<Vec<_>>().join(" ");
                d.fix_sql = index_sql(cx.engine(), &table, &cols, &[], n.facts().table_kind.as_deref());
            }
            None => {
                let fix = t!("insight.scan_filter.fix.index", columns = format::list(&cols));
                d.fix = std::iter::once(fix).chain(order).collect::<Vec<_>>().join(" ");
                d.fix_sql = index_sql(cx.engine(), &table, &cols, &[], n.facts().table_kind.as_deref());
            }
        }
        d.verify = t!("insight.scan_filter.verify", table = table_leaf(&table));
        out.push(d);
    }
    out
}

pub(super) fn big_sort(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    // An actual plan with operator timings says what the sort took; its cost share was estimated for the rows the
    // optimizer expected, which can be many times the rows it sorted.
    let measured = crate::actual::self_times(cx.plan);
    let total_ms: f64 = measured.as_ref().map_or(0.0, |t| t.values().sum());
    for n in cx.nodes.iter().filter(|n| n.kind == NodeKind::Sort) {
        if n.warnings.contains(&PlanWarning::Spill)
            || n.detail.as_deref().is_some_and(|d| d.contains("temporary table"))
        {
            continue; // spill and temp-table-sort say more
        }
        // Rows expected from a scan planned on a default guess (a parameter compared by range in a generic plan), or a
        // sort run once per row of one, are the guess, not the data: no row count to state, nor a share of the work
        // built on it, until a run measures them.
        if n.act_rows.is_none() && cx.guessed.contains(&n.id) {
            continue;
        }
        let rows = n.children.first().map_or(rows_out(n), rows_out).max(rows_out(n));
        let share = match &measured {
            Some(times) if total_ms > 0.0 => times.get(&n.id).map_or(0.0, |ms| (ms / total_ms).min(1.0)),
            _ => cx.share(n),
        };
        if share < 0.2 || rows < 50_000.0 {
            continue;
        }
        let mut d = Draft::new("big-sort", InsightArea::Index, share, vec![n.id]);
        d.title = if cx.states_share(&[n.id]) {
            t!("insight.big_sort.title_share", n = rows, count = format::count(rows), share = format::percent(share))
        } else {
            t!("insight.big_sort.title", n = rows, count = format::count(rows))
        };
        let key =
            n.detail.as_deref().and_then(|d| d.split(" · ").find(|p| p.starts_with("sort key")).map(str::to_string));
        d.detail = match key {
            Some(key) => t!("insight.big_sort.detail_key", key = key),
            None => t!("insight.big_sort.detail"),
        };
        d.fix = t!("insight.big_sort.fix");
        d.verify = t!("insight.big_sort.verify");
        out.push(d);
    }
    out
}

pub(super) fn eager_spool(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| n.op == "Index Spool" && n.detail.as_deref() == Some("Eager Spool")) {
        let table = first_object(n);
        // The spool's keys as columns of the table: an optimizer value (Expr1003, computed below the spool) is no
        // column an index on the table could have.
        let keys: Vec<String> = filter_columns(n.seek_predicate.as_deref().unwrap_or(""))
            .into_iter()
            .filter(|k| is_column_operand(k))
            .collect();
        let include = output_columns(n);
        let rows = n.children.first().map_or(0.0, rows_out);
        let impact = cx.subtree(n);
        let mut d = Draft::new("eager-spool", InsightArea::Index, impact, vec![n.id]);
        d.floor = Severity::Medium;
        let (named, leaf) = (known(table.is_some()), table.as_deref().map(table_leaf).unwrap_or_default());
        d.title = t!("insight.eager_spool.title", named = named, table = &leaf);
        d.detail = if keys.is_empty() {
            t!("insight.eager_spool.detail", n = rows, rows = format::count(rows), named = named, table = &leaf)
        } else {
            t!(
                "insight.eager_spool.detail_keys",
                n = rows,
                rows = format::count(rows),
                named = named,
                table = &leaf,
                keys = format::list(&keys)
            )
        };
        d.fix = if keys.is_empty() { t!("insight.eager_spool.fix_computed") } else { t!("insight.eager_spool.fix") };
        // Nothing to create on a table the plan doesn't name.
        d.fix_sql = table.and_then(|table| index_sql(Engine::Mssql, &table, &keys, &include, None));
        d.verify = t!("insight.eager_spool.verify");
        out.push(d);
    }
    out
}

/// SQLite built a throwaway index for this statement (`AUTOMATIC COVERING INDEX ON t(x)`): by its own account no index
/// fits, so it builds one on each run and drops it. Its plans carry no row counts, so how much that costs is unknown
/// here: no floor, unlike SQL Server's eager spool. A scan weighs it by the statement's share of database time.
pub(super) fn automatic_index(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for EngineBuiltIndex { node, table, keys } in engine_built_indexes(cx.plan) {
        let mut d = Draft::new("automatic-index", InsightArea::Index, 1.0, vec![node]);
        d.fixed_weight = true;
        let leaf = table_leaf(&table);
        d.title = t!("insight.automatic_index.title", table = &leaf);
        // What the index is for: a join, or the statement's own search.
        let serves = if cx.statement().contains("join") { "join" } else { "search" };
        d.detail = if keys.is_empty() {
            t!("insight.automatic_index.detail", table = &leaf, serves = serves)
        } else {
            t!("insight.automatic_index.detail_keys", table = &leaf, serves = serves, keys = format::list(&keys))
        };
        d.fix = t!("insight.automatic_index.fix");
        d.fix_sql = index_sql(Engine::Sqlite, &table, &keys, &[], None);
        d.verify = t!("insight.automatic_index.verify");
        out.push(d);
    }
    out
}

pub(super) fn many_to_many(cx: &Cx) -> Vec<Draft> {
    cx.nodes
        .iter()
        .filter(|n| n.op == "Merge Join" && (n.facts().many_to_many == Some(true)))
        .take(1)
        .map(|n| {
            let mut d = Draft::new("many-to-many", InsightArea::Index, cx.share(n), vec![n.id]);
            d.cap = Severity::Medium;
            d.title = t!("insight.many_to_many.title");
            d.detail = t!("insight.many_to_many.detail");
            d.fix = t!("insight.many_to_many.fix");
            d.verify = t!("insight.many_to_many.verify");
            d
        })
        .collect()
}

pub(super) fn join_buffer(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx
        .nodes
        .iter()
        .filter(|n| n.facts().join_buffer.is_some() && (n.predicate.is_some() || n.seek_predicate.is_some()))
    {
        let table = what(n);
        let how = n.facts().join_buffer.clone().unwrap_or_default();
        // One read of a small table into the buffer costs too little for an index to be worth having. Batched Key
        // Access buffers the rows joined so far and looks each one up through an index: no full read to speak of.
        if rows_read(n) < MANY_ROWS || how.contains("Batched Key Access") {
            continue;
        }
        let cond = n.predicate.clone().unwrap_or_default();
        let own = table_leaf(&table).to_ascii_lowercase();
        let alias = n.facts().table_alias.as_ref().map(|a| a.to_ascii_lowercase());
        // Columns of this table in the join condition: `t`.`col` where t is this table's name or alias.
        let cols: Vec<String> = cond
            .split(" and ")
            .flat_map(|part| {
                part.split(['=', '<', '>'])
                    .map(str::trim)
                    .filter(|side| {
                        let l = side.to_ascii_lowercase();
                        l.contains(&format!("{own}`."))
                            || l.contains(&format!("{own}."))
                            || alias
                                .as_ref()
                                .is_some_and(|a| l.contains(&format!("{a}`.")) || l.contains(&format!("{a}.")))
                    })
                    .map(column_leaf)
                    .collect::<Vec<_>>()
            })
            .collect();
        let impact = cx.share(n).max(cx.subtree(n) * 0.5);
        let mut d = Draft::new("join-buffer", InsightArea::Index, impact, vec![n.id]);
        let leaf = table_leaf(&table);
        let (named, columns) = (known(!cols.is_empty()), format::list(&cols));
        let read = rows_read(n);
        // MySQL listed indexes it could use on this table, yet joins through the buffer: it judged a full read
        // cheaper, or those indexes serve another condition. Either way the plan doesn't show an index is missing.
        match (!n.facts().possible_index_names.is_empty()).then(|| n.facts().possible_index_names.join(", ")) {
            Some(keys) => {
                d.title = t!("insight.join_buffer.title_possible", table = &leaf, how = &how);
                d.detail = t!(
                    "insight.join_buffer.detail_possible",
                    keys = &keys,
                    table = &leaf,
                    n = read,
                    rows = format::count(read)
                );
                d.fix = t!(
                    "insight.join_buffer.fix_possible",
                    keys = &keys,
                    named = named,
                    columns = &columns,
                    table = &leaf
                );
                d.fix_sql =
                    n.object.as_ref().map(|t| Engine::Mysql.dialect().refresh_statistics(&qualified(Engine::Mysql, t)));
            }
            None => {
                // An index pays for a join when the rows to match are few next to the table: one lookup each against
                // one full read. With as many to match as a tenth of what it reads, the hash join is the better plan.
                let before = cx.parent(n).and_then(|p| {
                    let at = p.children.iter().position(|c| c.id == n.id)?;
                    (at > 0).then(|| rows_out(&p.children[at - 1]))
                });
                if before.is_some_and(|outer| outer * 10.0 >= rows_read(n)) {
                    continue;
                }
                d.title = t!("insight.join_buffer.title", table = &leaf, how = &how);
                d.detail = t!("insight.join_buffer.detail", table = &leaf, n = read, rows = format::count(read));
                d.fix = t!("insight.join_buffer.fix", table = &leaf, named = named, columns = &columns);
                d.fix_sql = index_sql(Engine::Mysql, &table, &cols, &[], None);
            }
        }
        d.verify = t!("insight.join_buffer.verify", table = &leaf);
        out.push(d);
    }
    out
}
