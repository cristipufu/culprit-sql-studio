//! Index DDL, maintenance, `DROP`/`TRUNCATE`, and active workload reference checks.

use culprit_core::{
    Actionability, Category, Confidence, Engine, Finding, Fix, Severity, Snapshot, SubjectKind, TableStat, t,
};

use crate::support::{base_score, finding, subject, supports_online_index};

use super::lex::{
    append_sql_clause, compact_sql, contains_identifier, ends_with_line_comment, find_keyword_outside_comments,
    parse_paren_list, tokenize,
};
use super::{
    GB, HIGH_TABLE_ROWS, find_table, is_large_table, postgres_qualified_index, push_table_metrics,
    supports_concurrent_index, table_bytes, table_rows, table_size_label, unquote_ident,
};

pub(super) fn check_concurrent_in_tx(
    snapshot: &Snapshot,
    stmt: &str,
    tokens: &[String],
    upper: &[String],
    in_transaction: bool,
    findings: &mut Vec<Finding>,
) {
    if !in_transaction || snapshot.server.engine != Engine::Postgres {
        return;
    }
    let Some(conc_pos) = upper.iter().position(|t| t == "CONCURRENTLY") else {
        return;
    };
    let head = upper.first().map(String::as_str).unwrap_or("");
    if !matches!(head, "CREATE" | "DROP" | "REINDEX") {
        return;
    }
    let mut target_pos = conc_pos + 1;
    if upper.get(target_pos).map(String::as_str) == Some("IF") {
        if upper.get(target_pos + 1).map(String::as_str) == Some("NOT")
            && upper.get(target_pos + 2).map(String::as_str) == Some("EXISTS")
        {
            target_pos += 3;
        } else if upper.get(target_pos + 1).map(String::as_str) == Some("EXISTS") {
            target_pos += 2;
        }
    }
    if upper.get(target_pos).map(String::as_str) == Some("ON") {
        target_pos += 1;
        if upper.get(target_pos).map(String::as_str) == Some("ONLY") {
            target_pos += 1;
        }
    }
    let named =
        tokens.get(target_pos).filter(|t| !matches!(t.as_str(), "(" | ")" | ";" | ",")).map(|s| unquote_ident(s));
    // The finding's id and subject name the statement's target; one without a name is "index" there.
    let target = named.clone().unwrap_or_else(|| "index".into());
    let kind_word = if head == "REINDEX" && upper[..conc_pos].iter().any(|t| t == "TABLE") { "TABLE" } else { "INDEX" };
    let mut f = finding(
        "sql-lint",
        &format!("concurrent-in-tx:{target}"),
        Category::Indexes,
        subject(SubjectKind::Index, &target),
    );
    f.actionability = Actionability::NeedsAttention;
    f.confidence = Confidence::High;
    f.severity = Severity::High;
    f.impact.score = base_score(f.severity);
    f.impact.label = t!("view.sql_lint.concurrent_in_tx.impact");
    f.title = t!(
        "view.sql_lint.concurrent_in_tx.title",
        statement = format!("{head} {kind_word} CONCURRENTLY"),
        target = named.unwrap_or_else(|| t!("view.sql_lint.concurrent_in_tx.unnamed"))
    );
    f.summary = t!("view.sql_lint.concurrent_in_tx.summary", sql = compact_sql(stmt));
    f.fix = Some(Fix { summary: t!("view.sql_lint.concurrent_in_tx.fix"), sql: None, cost: None });
    f.verify = vec![t!("view.sql_lint.concurrent_in_tx.verify")];
    findings.push(f);
}

pub(super) fn check_drop_or_truncate(
    snapshot: &Snapshot,
    stmt: &str,
    tokens: &[String],
    upper: &[String],
    in_transaction: bool,
    findings: &mut Vec<Finding>,
) {
    match upper.first().map(String::as_str) {
        Some("DROP") if upper.get(1).map(String::as_str) == Some("INDEX") => {
            let mut pos = 2;
            if upper.get(pos).map(String::as_str) == Some("IF")
                && upper.get(pos + 1).map(String::as_str) == Some("EXISTS")
            {
                pos += 2;
            }
            let Some(raw_idx) = tokens.get(pos) else { return };
            let idx_clean = unquote_ident(raw_idx);
            let idx_leaf = idx_clean.rsplit('.').next().unwrap_or(&idx_clean);
            let on_table = upper
                .iter()
                .position(|t| t == "ON")
                .and_then(|on_pos| tokens.get(on_pos + 1))
                .and_then(|raw_tbl| find_table(snapshot, raw_tbl));
            let Some(ix) = snapshot.indexes.iter().find(|i| {
                i.name.eq_ignore_ascii_case(idx_leaf)
                    && on_table.is_none_or(|t| {
                        i.schema.eq_ignore_ascii_case(&t.schema) && i.table.eq_ignore_ascii_case(&t.name)
                    })
            }) else {
                return;
            };
            let Some(table) = snapshot.table(&ix.schema, &ix.table).filter(|t| is_large_table(t)) else {
                return;
            };
            let engine = snapshot.server.engine;
            let is_blocking = match engine {
                Engine::Postgres => {
                    supports_concurrent_index(snapshot, Some(table)) && !upper.iter().any(|t| t == "CONCURRENTLY")
                }
                Engine::Mysql => !has_mysql_online_ddl(upper),
                Engine::Mssql | Engine::Yugabyte | Engine::Sqlite => false,
            };
            if !is_blocking {
                return;
            }
            let rows = table_rows(table);
            let bytes = table_bytes(table);
            let qual = table.qualified();
            let mut f = finding(
                "sql-lint",
                &format!("blocking-drop-index:{}", ix.name),
                Category::Indexes,
                subject(SubjectKind::Index, format!("{qual}.{}", ix.name)),
            );
            f.actionability = Actionability::NeedsAttention;
            f.confidence = Confidence::High;
            f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
            f.impact.score = base_score(f.severity);
            let size = table_size_label(rows, bytes);
            f.impact.label = size.clone();
            let sql = compact_sql(stmt);
            if engine == Engine::Mysql {
                f.title =
                    t!("view.sql_lint.drop_index.mysql.title", index = &ix.name, table = &table.name, size = &size);
                f.summary = t!(
                    "view.sql_lint.drop_index.mysql.summary",
                    sql = sql,
                    index = &ix.name,
                    table = &qual,
                    size = size
                );
                f.fix = Some(Fix {
                    summary: t!("view.sql_lint.drop_index.mysql.fix"),
                    sql: Some(Engine::Mysql.dialect().drop_index(&ix.schema, &ix.table, &ix.name)),
                    cost: None,
                });
                f.verify = vec![t!("view.sql_lint.drop_index.mysql.verify")];
            } else {
                f.title =
                    t!("view.sql_lint.drop_index.postgres.title", index = &ix.name, table = &table.name, size = &size);
                f.summary = t!(
                    "view.sql_lint.drop_index.postgres.summary",
                    sql = sql,
                    index = &ix.name,
                    table = &qual,
                    size = size
                );
                let multi_drop = upper.iter().any(|t| t == ",");
                let q_idx = postgres_qualified_index(&ix.schema, &ix.name);
                f.fix = Some(Fix {
                    summary: t!("view.sql_lint.drop_index.postgres.fix"),
                    sql: (!in_transaction && !multi_drop)
                        .then(|| format!("DROP INDEX CONCURRENTLY IF EXISTS {q_idx};")),
                    cost: Some(t!("view.sql_lint.drop_index.postgres.cost")),
                });
                f.verify = vec![t!("view.sql_lint.drop_index.postgres.verify")];
            }
            push_table_metrics(&mut f, rows, bytes);
            findings.push(f);
        }
        Some("DROP") if upper.get(1).map(String::as_str) == Some("TABLE") => {
            let mut pos = 2;
            if upper.get(pos).map(String::as_str) == Some("IF")
                && upper.get(pos + 1).map(String::as_str) == Some("EXISTS")
            {
                pos += 2;
            }
            if let Some(raw_table) = tokens.get(pos)
                && let Some(table) = find_table(snapshot, raw_table)
            {
                check_workload_references(snapshot, stmt, table, None, "DROP TABLE", findings);
            }
        }
        Some("TRUNCATE") => {
            let mut pos = 1;
            if upper.get(pos).map(String::as_str) == Some("TABLE") {
                pos += 1;
            }
            if upper.get(pos).map(String::as_str) == Some("ONLY") {
                pos += 1;
            }
            let Some(raw_table) = tokens.get(pos) else { return };
            let Some(table) = find_table(snapshot, raw_table).filter(|t| is_large_table(t)) else {
                return;
            };
            let rows = table_rows(table);
            let bytes = table_bytes(table);
            let qual = table.qualified();
            let mut f = finding(
                "sql-lint",
                &format!("truncate:{qual}"),
                Category::Maintenance,
                subject(SubjectKind::Table, &qual),
            );
            f.actionability = Actionability::NeedsAttention;
            f.confidence = Confidence::High;
            f.severity = Severity::High;
            f.impact.score = base_score(f.severity);
            let size = table_size_label(rows, bytes);
            f.impact.label = size.clone();
            f.title = t!("view.sql_lint.truncate.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.truncate.summary", sql = compact_sql(stmt), table = &qual, size = size);
            push_table_metrics(&mut f, rows, bytes);
            f.verify = vec![t!("view.sql_lint.truncate.verify")];
            findings.push(f);
        }
        _ => {}
    }
}

pub(super) fn check_workload_references(
    snapshot: &Snapshot,
    stmt: &str,
    table: &TableStat,
    column: Option<&str>,
    action: &str,
    findings: &mut Vec<Finding>,
) {
    let active: Vec<&culprit_core::QueryStat> = crate::support::queries_touching(snapshot, &table.schema, &table.name)
        .into_iter()
        .filter(|q| {
            !crate::support::is_monitoring_query(q)
                && !crate::query_is_history(snapshot, q)
                && column.is_none_or(|col| contains_identifier(&q.text, col))
        })
        .collect();
    if active.is_empty() {
        return;
    }
    let qual = table.qualified();
    let target = match column {
        Some(col) => format!("{qual}.{col}"),
        None => qual.clone(),
    };
    let target_short = match column {
        Some(col) => format!("{}.{col}", table.name),
        None => table.name.clone(),
    };
    let mut f =
        finding("sql-lint", &format!("workload-break:{target}"), Category::Queries, subject(SubjectKind::Table, &qual));
    f.actionability = Actionability::NeedsAttention;
    f.confidence = Confidence::High;
    f.severity = Severity::High;
    f.impact.score = base_score(f.severity);
    // A count of statements, written as it is ("1234", not "1.2K").
    let (n, count) = (active.len(), active.len().to_string());
    f.impact.label = t!("view.sql_lint.workload_break.impact", n = n, count = &count);
    f.queries = crate::support::query_refs(&active);
    f.title = t!("view.sql_lint.workload_break.title", action = action, target = &target_short, n = n, count = &count);
    f.summary = t!(
        "view.sql_lint.workload_break.summary",
        sql = compact_sql(stmt),
        action = action,
        target = &target_short,
        n = n,
        count = count
    );
    f.fix = Some(Fix {
        summary: t!("view.sql_lint.workload_break.fix", target = &target_short, action = action),
        sql: None,
        cost: None,
    });
    f.verify = vec![t!("view.sql_lint.workload_break.verify", target = &target_short, action = action)];
    findings.push(f);
}

pub(super) fn check_maintenance(
    snapshot: &Snapshot,
    stmt: &str,
    tokens: &[String],
    upper: &[String],
    in_transaction: bool,
    findings: &mut Vec<Finding>,
) {
    if snapshot.server.engine != Engine::Postgres {
        return;
    }
    match upper.first().map(String::as_str) {
        Some("VACUUM") => {
            let mut pos = 1;
            let mut is_full = false;
            if upper.get(pos).map(String::as_str) == Some("(") {
                let mut depth = 0usize;
                for (i, tok) in upper.iter().enumerate().skip(pos) {
                    match tok.as_str() {
                        "(" => depth += 1,
                        ")" => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                pos = i + 1;
                                break;
                            }
                        }
                        "FULL" if depth == 1 => {
                            is_full = true;
                        }
                        _ => {}
                    }
                }
            } else {
                while let Some(tok) = upper.get(pos).map(String::as_str) {
                    if tok == "FULL" {
                        is_full = true;
                        pos += 1;
                    } else if matches!(tok, "FREEZE" | "VERBOSE" | "ANALYZE" | "ANALYSE") {
                        pos += 1;
                    } else {
                        break;
                    }
                }
            }
            if !is_full {
                return;
            }
            let Some(raw_table) = tokens.get(pos) else { return };
            let Some(table) = find_table(snapshot, raw_table).filter(|t| is_large_table(t)) else {
                return;
            };
            let rows = table_rows(table);
            let bytes = table_bytes(table);
            let qual = table.qualified();
            let mut f = finding(
                "sql-lint",
                &format!("vacuum-full:{qual}"),
                Category::Maintenance,
                subject(SubjectKind::Table, &qual),
            );
            f.actionability = Actionability::NeedsAttention;
            f.confidence = Confidence::High;
            f.severity = Severity::High;
            f.impact.score = base_score(f.severity);
            let size = table_size_label(rows, bytes);
            f.impact.label = size.clone();
            f.title = t!("view.sql_lint.vacuum_full.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.vacuum_full.summary", sql = compact_sql(stmt), table = &qual, size = size);
            f.fix = Some(Fix {
                summary: t!("view.sql_lint.vacuum_full.fix"),
                sql: Some(format!("VACUUM (ANALYZE) {qual};")),
                cost: None,
            });
            push_table_metrics(&mut f, rows, bytes);
            f.verify = vec![t!("view.sql_lint.vacuum_full.verify")];
            findings.push(f);
        }
        Some("CLUSTER") => {
            let mut pos = 1;
            if upper.get(pos).map(String::as_str) == Some("VERBOSE") {
                pos += 1;
            }
            let Some(raw_table) = tokens.get(pos) else { return };
            let Some(table) = find_table(snapshot, raw_table).filter(|t| is_large_table(t)) else {
                return;
            };
            let rows = table_rows(table);
            let bytes = table_bytes(table);
            let qual = table.qualified();
            let mut f = finding(
                "sql-lint",
                &format!("cluster:{qual}"),
                Category::Maintenance,
                subject(SubjectKind::Table, &qual),
            );
            f.actionability = Actionability::NeedsAttention;
            f.confidence = Confidence::High;
            f.severity = Severity::High;
            f.impact.score = base_score(f.severity);
            let size = table_size_label(rows, bytes);
            f.impact.label = size.clone();
            f.title = t!("view.sql_lint.cluster.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.cluster.summary", sql = compact_sql(stmt), table = &qual, size = size);
            f.fix = Some(Fix { summary: t!("view.sql_lint.cluster.fix"), sql: None, cost: None });
            push_table_metrics(&mut f, rows, bytes);
            f.verify = vec![t!("view.sql_lint.cluster.verify")];
            findings.push(f);
        }
        Some("REINDEX") => {
            if upper.iter().any(|t| t == "CONCURRENTLY") {
                return;
            }
            let mut pos = 1;
            if upper.get(pos).map(String::as_str) == Some("(") {
                let mut depth = 0usize;
                for (i, tok) in upper.iter().enumerate().skip(pos) {
                    match tok.as_str() {
                        "(" => depth += 1,
                        ")" => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                pos = i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            let kind = upper.get(pos).map(String::as_str).unwrap_or("");
            let Some(raw_target) = tokens.get(pos + 1) else { return };
            match kind {
                "TABLE" => {
                    let Some(table) = find_table(snapshot, raw_target).filter(|t| is_large_table(t)) else {
                        return;
                    };
                    if !supports_concurrent_index(snapshot, Some(table)) {
                        return;
                    }
                    let rows = table_rows(table);
                    let bytes = table_bytes(table);
                    let qual = table.qualified();
                    let mut f = finding(
                        "sql-lint",
                        &format!("blocking-reindex:{qual}"),
                        Category::Indexes,
                        subject(SubjectKind::Table, &qual),
                    );
                    f.actionability = Actionability::NeedsAttention;
                    f.confidence = Confidence::High;
                    f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
                    f.impact.score = base_score(f.severity);
                    let size = table_size_label(rows, bytes);
                    f.impact.label = size.clone();
                    f.title = t!("view.sql_lint.reindex_table.title", table = &table.name, size = &size);
                    f.summary =
                        t!("view.sql_lint.reindex_table.summary", sql = compact_sql(stmt), table = &qual, size = size);
                    f.fix = Some(Fix {
                        summary: t!("view.sql_lint.reindex_table.fix"),
                        sql: (!in_transaction).then(|| format!("REINDEX TABLE CONCURRENTLY {qual};")),
                        cost: Some(t!("view.sql_lint.reindex.cost")),
                    });
                    push_table_metrics(&mut f, rows, bytes);
                    f.verify = vec![t!("view.sql_lint.reindex_table.verify")];
                    findings.push(f);
                }
                "INDEX" => {
                    let idx_clean = unquote_ident(raw_target);
                    let idx_leaf = idx_clean.rsplit('.').next().unwrap_or(&idx_clean);
                    let Some(ix) = snapshot.indexes.iter().find(|i| i.name.eq_ignore_ascii_case(idx_leaf)) else {
                        return;
                    };
                    let Some(table) = snapshot.table(&ix.schema, &ix.table).filter(|t| is_large_table(t)) else {
                        return;
                    };
                    if !supports_concurrent_index(snapshot, Some(table)) {
                        return;
                    }
                    let rows = table_rows(table);
                    let bytes = table_bytes(table);
                    let qual = table.qualified();
                    let mut f = finding(
                        "sql-lint",
                        &format!("blocking-reindex:{}", ix.name),
                        Category::Indexes,
                        subject(SubjectKind::Index, format!("{qual}.{}", ix.name)),
                    );
                    f.actionability = Actionability::NeedsAttention;
                    f.confidence = Confidence::High;
                    f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
                    f.impact.score = base_score(f.severity);
                    let size = table_size_label(rows, bytes);
                    f.impact.label = size.clone();
                    f.title =
                        t!("view.sql_lint.reindex_index.title", index = &ix.name, table = &table.name, size = &size);
                    f.summary = t!(
                        "view.sql_lint.reindex_index.summary",
                        sql = compact_sql(stmt),
                        index = &ix.name,
                        table = &qual,
                        size = size
                    );
                    let q_idx = postgres_qualified_index(&ix.schema, &ix.name);
                    f.fix = Some(Fix {
                        summary: t!("view.sql_lint.reindex_index.fix"),
                        sql: (!in_transaction).then(|| format!("REINDEX INDEX CONCURRENTLY {q_idx};")),
                        cost: Some(t!("view.sql_lint.reindex.cost")),
                    });
                    push_table_metrics(&mut f, rows, bytes);
                    f.verify = vec![t!("view.sql_lint.reindex_index.verify")];
                    findings.push(f);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(super) fn check_create_index(
    snapshot: &Snapshot,
    stmt: &str,
    tokens: &[String],
    in_transaction: bool,
    findings: &mut Vec<Finding>,
) {
    let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    if upper.first().map(String::as_str) != Some("CREATE") {
        return;
    }
    let Some(idx_pos) = upper.iter().position(|t| t == "INDEX") else {
        return;
    };
    if idx_pos > 3 {
        return;
    }
    let has_concurrently = upper.iter().any(|t| t == "CONCURRENTLY");
    let Some(on_pos) = upper.iter().position(|t| t == "ON") else {
        return;
    };
    let after_on = if upper.get(on_pos + 1).map(String::as_str) == Some("ONLY") { on_pos + 2 } else { on_pos + 1 };
    let Some(raw_table) = tokens.get(after_on) else {
        return;
    };
    let Some(table) = find_table(snapshot, raw_table) else {
        return;
    };
    if !is_large_table(table) {
        return;
    }
    let engine = snapshot.server.engine;
    let is_blocking = match engine {
        Engine::Postgres => supports_concurrent_index(snapshot, Some(table)) && !has_concurrently,
        Engine::Mssql => supports_online_index(snapshot) && !has_mssql_online_on(&upper),
        Engine::Mysql => !has_mysql_online_ddl(&upper),
        Engine::Yugabyte | Engine::Sqlite => false,
    };
    if !is_blocking {
        return;
    }
    let rows = table_rows(table);
    let bytes = table_bytes(table);
    let qual = table.qualified();
    let mut f =
        finding("sql-lint", &format!("blocking-index:{qual}"), Category::Indexes, subject(SubjectKind::Table, &qual));
    f.actionability = Actionability::NeedsAttention;
    f.confidence = Confidence::High;
    f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
    f.impact.score = base_score(f.severity);
    let size = table_size_label(rows, bytes);
    f.impact.label = size.clone();
    let sql = compact_sql(stmt);
    match engine {
        Engine::Postgres => {
            f.title = t!("view.sql_lint.create_index.postgres.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.create_index.postgres.summary", sql = sql, table = &qual, size = size);
            f.fix = Some(Fix {
                summary: t!("view.sql_lint.create_index.postgres.fix"),
                sql: (!in_transaction).then(|| suggest_concurrent_index(stmt)).flatten(),
                cost: Some(t!("view.sql_lint.create_index.postgres.cost")),
            });
        }
        Engine::Mssql => {
            f.title = t!("view.sql_lint.create_index.mssql.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.create_index.mssql.summary", sql = sql, table = &qual, size = size);
            f.fix = Some(Fix {
                summary: t!("view.sql_lint.create_index.mssql.fix"),
                sql: suggest_mssql_online_index(stmt, &upper),
                cost: Some(t!("view.sql_lint.create_index.mssql.cost")),
            });
        }
        Engine::Mysql => {
            f.title = t!("view.sql_lint.create_index.mysql.title", table = &table.name, size = &size);
            f.summary = t!("view.sql_lint.create_index.mysql.summary", sql = sql, table = &qual, size = size);
            f.fix = Some(Fix {
                summary: t!("view.sql_lint.create_index.mysql.fix"),
                sql: suggest_mysql_online_index(stmt, &upper),
                cost: Some(t!("view.sql_lint.create_index.mysql.cost")),
            });
        }
        Engine::Yugabyte | Engine::Sqlite => {}
    }
    push_table_metrics(&mut f, rows, bytes);
    f.verify = vec![t!("view.sql_lint.create_index.verify")];
    findings.push(f);
}

pub(super) fn collect_script_indexes(statements: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for stmt in statements {
        let tokens = tokenize(stmt);
        let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
        match upper.first().map(String::as_str) {
            Some("CREATE") => {
                if !upper.iter().take(4).any(|t| t == "INDEX") {
                    continue;
                }
                let Some(on_pos) = upper.iter().position(|t| t == "ON") else {
                    continue;
                };
                let after_on =
                    if upper.get(on_pos + 1).map(String::as_str) == Some("ONLY") { on_pos + 2 } else { on_pos + 1 };
                let Some(table) = tokens.get(after_on) else {
                    continue;
                };
                let mut col_start = after_on + 1;
                if upper.get(col_start).map(String::as_str) == Some("USING") {
                    col_start += 2;
                }
                if let Some((cols, _)) = parse_paren_list(&tokens, col_start) {
                    out.push((table.clone(), cols));
                }
            }
            Some("ALTER") if upper.get(1).map(String::as_str) == Some("TABLE") => {
                let mut pos = 2;
                if upper.get(pos).map(String::as_str) == Some("IF")
                    && upper.get(pos + 1).map(String::as_str) == Some("EXISTS")
                {
                    pos += 2;
                }
                if upper.get(pos).map(String::as_str) == Some("ONLY") {
                    pos += 1;
                }
                let Some(table) = tokens.get(pos) else {
                    continue;
                };
                for (clause_tokens, clause_upper) in
                    super::lex::split_top_level_commas(&tokens[pos + 1..], &upper[pos + 1..])
                {
                    if clause_upper.first().map(String::as_str) != Some("ADD") {
                        continue;
                    }
                    let mut idx = 1;
                    if matches!(clause_upper.get(idx).map(String::as_str), Some("UNIQUE" | "FULLTEXT" | "SPATIAL")) {
                        idx += 1;
                    }
                    if !matches!(clause_upper.get(idx).map(String::as_str), Some("INDEX" | "KEY")) {
                        continue;
                    }
                    idx += 1;
                    if clause_upper.get(idx).map(String::as_str) == Some("IF")
                        && clause_upper.get(idx + 1).map(String::as_str) == Some("NOT")
                        && clause_upper.get(idx + 2).map(String::as_str) == Some("EXISTS")
                    {
                        idx += 3;
                    }
                    // Optional index name before '(' or 'USING'.
                    if clause_tokens.get(idx).is_some_and(|t| t != "(" && !t.eq_ignore_ascii_case("USING")) {
                        idx += 1;
                    }
                    if clause_upper.get(idx).map(String::as_str) == Some("USING") {
                        idx += 2;
                    }
                    if let Some((cols, _)) = parse_paren_list(clause_tokens, idx) {
                        out.push((table.clone(), cols));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(super) fn has_mssql_online_on(upper: &[String]) -> bool {
    upper.windows(3).any(|w| w == ["ONLINE", "=", "ON"])
}

pub(super) fn has_mysql_online_ddl(upper: &[String]) -> bool {
    upper.windows(3).any(|w| w == ["LOCK", "=", "NONE"] || w == ["ALGORITHM", "=", "INSTANT"])
        || upper.windows(2).any(|w| w == ["LOCK", "NONE"] || w == ["ALGORITHM", "INSTANT"])
}

fn suggest_mysql_online_index(stmt: &str, upper: &[String]) -> Option<String> {
    if upper.iter().any(|t| matches!(t.as_str(), "ALGORITHM" | "LOCK")) {
        return None;
    }
    Some(append_sql_clause(stmt, " ALGORITHM=INPLACE LOCK=NONE;"))
}

fn suggest_concurrent_index(stmt: &str) -> Option<String> {
    let trimmed = stmt.trim().trim_end_matches(';').trim_end();
    let create_pos = find_keyword_outside_comments(trimmed, "CREATE")?;
    let idx_pos = find_keyword_outside_comments(trimmed, "INDEX")?;
    if idx_pos <= create_pos || find_keyword_outside_comments(trimmed, "CONCURRENTLY").is_some() {
        return None;
    }
    let is_unique = find_keyword_outside_comments(&trimmed[create_pos..idx_pos], "UNIQUE").is_some();
    let rest = trimmed[idx_pos + "INDEX".len()..].trim_start();
    if rest.is_empty() {
        return None;
    }
    let head = if is_unique { "CREATE UNIQUE INDEX CONCURRENTLY " } else { "CREATE INDEX CONCURRENTLY " };
    let sep = if ends_with_line_comment(rest) { "\n;" } else { ";" };
    Some(format!("{head}{rest}{sep}"))
}

fn suggest_mssql_online_index(stmt: &str, upper: &[String]) -> Option<String> {
    if upper.iter().any(|t| t == "WITH") {
        return None;
    }
    Some(append_sql_clause(stmt, " WITH (ONLINE = ON);"))
}
