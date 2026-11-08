//! Offline migration and DDL SQL linter against a saved [`Snapshot`].
//!
//! Checks a proposed migration script or DDL statement against the real table sizes, row counts, indexes and column
//! types in a saved snapshot without touching the database:
//! - Blocking `CREATE INDEX` without `CONCURRENTLY` (PostgreSQL) or `ONLINE = ON` (SQL Server) on large tables.
//! - `ALTER TABLE ... ADD FOREIGN KEY` where the referencing columns have no supporting index in the snapshot (or in
//!   the migration itself) or have a column type mismatch with the referenced parent column.
//! - `ALTER TABLE` operations that hold `ACCESS EXCLUSIVE` / schema-modification locks while scanning or rewriting a
//!   large table (`ALTER COLUMN ... TYPE`, `SET NOT NULL`).

mod alter_table;
mod index_ddl;
mod lex;
#[cfg(test)]
mod tests;

use culprit_core::{Engine, Finding, Metric, Snapshot, StatementEvidence, TableStat, Unit, format, t};

use alter_table::{check_alter_table, collect_validated_constraint_tables};
use index_ddl::{
    check_concurrent_in_tx, check_create_index, check_drop_or_truncate, check_maintenance, collect_script_indexes,
};
use lex::{rewrite_preserving_comments, split_statement_spans, split_statements, tokenize};

pub(super) const LARGE_TABLE_ROWS: f64 = 100_000.0;
pub(super) const HIGH_TABLE_ROWS: f64 = 1_000_000.0;
pub(super) const MB: f64 = 1024.0 * 1024.0;
pub(super) const GB: f64 = 1024.0 * MB;
pub(super) const LARGE_TABLE_BYTES: f64 = 100.0 * MB;

pub(super) struct ScriptContext<'a> {
    pub(super) created_indexes: &'a [(String, Vec<String>)],
    pub(super) validated_tables: &'a [String],
    pub(super) in_transaction: bool,
}

/// Lint one or more SQL statements (such as a migration file) against the catalog and table sizes in `snapshot`.
pub fn lint_sql(snapshot: &Snapshot, sql: &str) -> Vec<Finding> {
    let spans = split_statement_spans(sql);
    let statements: Vec<String> = spans.iter().map(|(_, _, statement)| statement.clone()).collect();
    let created_indexes = collect_script_indexes(&statements);
    let validated_tables = collect_validated_constraint_tables(&statements);
    let mut findings = Vec::new();
    let mut in_transaction = false;
    let (mut previous, mut utf16, mut line) = (0, 0, 1);
    for (start, end, stmt) in &spans {
        let prefix = &sql[previous..*start];
        utf16 += prefix.encode_utf16().count();
        line += prefix.bytes().filter(|byte| *byte == b'\n').count();
        previous = *start;
        let source = &sql[*start..*end];
        let evidence =
            StatementEvidence { sql: source.into(), start: utf16, end: utf16 + source.encode_utf16().count(), line };
        let first = findings.len();
        let ctx =
            ScriptContext { created_indexes: &created_indexes, validated_tables: &validated_tables, in_transaction };
        lint_single_statement(snapshot, stmt, &ctx, &mut in_transaction, &mut findings);
        for finding in &mut findings[first..] {
            finding.facts.statement = Some(evidence.clone());
        }
    }
    findings
}

/// Apply all safe DDL auto-fixes (`Fix.sql`) recommended by [`lint_sql`] to `sql`, returning `(rewritten_sql, fixes_applied)`.
pub fn apply_sql_fixes(snapshot: &Snapshot, sql: &str) -> (String, usize) {
    let spans = split_statement_spans(sql);
    if spans.is_empty() {
        return (sql.to_string(), 0);
    }
    let statements: Vec<String> = spans.iter().map(|(_, _, s)| s.clone()).collect();
    let mut created_indexes = collect_script_indexes(&statements);
    let validated_tables = collect_validated_constraint_tables(&statements);

    let mut out = String::with_capacity(sql.len() + 128);
    let mut last_pos = 0usize;
    let mut applied = 0usize;
    let mut in_transaction = false;

    for (start, end, stmt) in &spans {
        let mut stmt_findings = Vec::new();
        {
            let ctx = ScriptContext {
                created_indexes: &created_indexes,
                validated_tables: &validated_tables,
                in_transaction,
            };
            lint_single_statement(snapshot, stmt, &ctx, &mut in_transaction, &mut stmt_findings);
        }

        out.push_str(&sql[last_pos..*start]);

        let mut emitted_fk_fixes = Vec::<String>::new();
        for f in &stmt_findings {
            if f.id.starts_with("sql-lint:unindexed-fk:")
                && !(in_transaction && snapshot.server.engine == Engine::Postgres)
                && let Some(fix_sql) = f.fix.as_ref().and_then(|fix| fix.sql.as_deref())
            {
                let trimmed = fix_sql.trim();
                if !emitted_fk_fixes.iter().any(|prev| prev == trimmed) {
                    out.push_str(trimmed);
                    out.push('\n');
                    created_indexes.extend(collect_script_indexes(&split_statements(trimmed)));
                    emitted_fk_fixes.push(trimmed.to_string());
                    applied += 1;
                }
            }
        }

        let replacements: Vec<&str> = stmt_findings
            .iter()
            .filter(|f| !f.id.starts_with("sql-lint:unindexed-fk:"))
            .filter_map(|f| f.fix.as_ref().and_then(|fix| fix.sql.as_deref()))
            .collect();

        if let [rep_sql] = replacements.as_slice() {
            out.push_str(&rewrite_preserving_comments(&sql[*start..*end], rep_sql.trim()));
            applied += 1;
        } else {
            out.push_str(&sql[*start..*end]);
        }
        last_pos = *end;
    }
    out.push_str(&sql[last_pos..]);
    (out, applied)
}

fn lint_single_statement(
    snapshot: &Snapshot,
    stmt: &str,
    ctx: &ScriptContext<'_>,
    in_transaction: &mut bool,
    findings: &mut Vec<Finding>,
) {
    let tokens = tokenize(stmt);
    if tokens.is_empty() {
        return;
    }
    let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    if is_transaction_begin(&upper) {
        *in_transaction = true;
        return;
    }
    if is_transaction_end(&upper) {
        *in_transaction = false;
        return;
    }
    check_concurrent_in_tx(snapshot, stmt, &tokens, &upper, *in_transaction, findings);
    check_create_index(snapshot, stmt, &tokens, *in_transaction, findings);
    check_drop_or_truncate(snapshot, stmt, &tokens, &upper, *in_transaction, findings);
    check_maintenance(snapshot, stmt, &tokens, &upper, *in_transaction, findings);
    check_alter_table(snapshot, stmt, &tokens, ctx, findings);
}

fn is_transaction_begin(upper: &[String]) -> bool {
    (upper.first().map(String::as_str) == Some("BEGIN")
        && matches!(
            upper.get(1).map(String::as_str),
            None | Some(
                "WORK"
                    | "TRANSACTION"
                    | "TRAN"
                    | "ISOLATION"
                    | "READ"
                    | "DEFERRABLE"
                    | "NOT"
                    | "DEFERRED"
                    | "IMMEDIATE"
                    | "EXCLUSIVE"
                    | "DISTRIBUTED"
            )
        ))
        || (upper.first().map(String::as_str) == Some("START")
            && upper.get(1).map(String::as_str) == Some("TRANSACTION"))
}

fn is_transaction_end(upper: &[String]) -> bool {
    matches!(upper.first().map(String::as_str), Some("COMMIT" | "ROLLBACK" | "END"))
}

pub(super) fn unquote_ident(raw: &str) -> String {
    culprit_core::sql::unquote_identifier(raw)
}

/// Whether `table` on `snapshot` supports `CREATE/DROP/REINDEX ... CONCURRENTLY`.
/// Delegates to [`culprit_core::EngineSpec::table_dialect`] so ordinary PostgreSQL tables require `CONCURRENTLY` while
/// TimescaleDB-managed hypertables and chunks (and non-PostgreSQL engines) do not.
pub(super) fn supports_concurrent_index(snapshot: &Snapshot, table: Option<&TableStat>) -> bool {
    snapshot
        .server
        .engine
        .spec()
        .table_dialect(table.and_then(|t| t.kind.as_deref()))
        .drop_index("", "t", "i")
        .contains("CONCURRENTLY")
}

pub(super) fn find_table<'a>(snapshot: &'a Snapshot, raw: &str) -> Option<&'a TableStat> {
    snapshot.tables.iter().find(|t| table_matches(t, raw))
}

pub(super) fn table_matches(table: &TableStat, raw: &str) -> bool {
    if let Some(parts) = culprit_core::sql::identifier_parts(raw) {
        return match parts.as_slice() {
            [.., schema, name] => table.schema.eq_ignore_ascii_case(schema) && table.name.eq_ignore_ascii_case(name),
            [name] => table.name.eq_ignore_ascii_case(name),
            [] => false,
        };
    }
    let cleaned = unquote_ident(raw);
    if let Some((schema, name)) = cleaned.rsplit_once('.') {
        table.schema.eq_ignore_ascii_case(schema) && table.name.eq_ignore_ascii_case(name)
    } else {
        table.name.eq_ignore_ascii_case(&cleaned)
    }
}

pub(super) fn is_large_table(t: &TableStat) -> bool {
    table_rows(t) >= LARGE_TABLE_ROWS || table_bytes(t) >= LARGE_TABLE_BYTES
}

pub(super) fn table_rows(t: &TableStat) -> f64 {
    t.rows.or_else(|| t.storage.as_ref().and_then(|s| s.planner_rows)).unwrap_or(0.0)
}

pub(super) fn table_bytes(t: &TableStat) -> f64 {
    t.total_bytes.or(t.data_bytes).unwrap_or(0.0)
}

pub(super) fn table_size_label(rows: f64, bytes: f64) -> String {
    match (rows > 0.0, bytes > 0.0) {
        (true, true) => t!("view.sql_lint.size.rows_bytes", rows = format::count(rows), size = format::bytes(bytes)),
        (true, false) => t!("view.sql_lint.size.rows", rows = format::count(rows)),
        (false, true) => format::bytes(bytes),
        (false, false) => t!("view.sql_lint.size.unknown"),
    }
}

pub(super) fn push_table_metrics(f: &mut Finding, rows: f64, bytes: f64) {
    if rows > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("view.sql_lint.metric.rows"), rows, Unit::Rows));
    }
    if bytes > 0.0 {
        f.metrics.push(Metric::said(culprit_core::key!("view.sql_lint.metric.size"), bytes, Unit::Bytes));
    }
}

pub(super) fn postgres_qualified_index(schema: &str, name: &str) -> String {
    let d = Engine::Postgres.dialect();
    let q_name = d.quote(name);
    if schema.is_empty() || schema.eq_ignore_ascii_case("public") { q_name } else { d.qualified(&[schema, name]) }
}
