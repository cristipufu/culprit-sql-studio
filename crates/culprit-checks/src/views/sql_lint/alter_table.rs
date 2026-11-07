//! `ALTER TABLE` rewrites, foreign-key additions, constraint validation, and type-change checks.

use culprit_core::{
    Actionability, Category, Confidence, Engine, Finding, Fix, Severity, Snapshot, SubjectKind, TableStat,
};

use crate::support::{base_score, finding, subject};

use super::index_ddl::{check_workload_references, has_mysql_online_ddl};
use super::lex::{append_sql_clause, compact_sql, parse_paren_list, split_top_level_commas, tokenize};
use super::{
    GB, HIGH_TABLE_ROWS, ScriptContext, find_table, is_large_table, push_table_metrics, table_bytes, table_matches,
    table_rows, table_size_label, unquote_ident,
};

pub(super) fn collect_validated_constraint_tables(statements: &[String]) -> Vec<String> {
    let mut fk_constraints = Vec::new();
    for stmt in statements {
        let tokens = tokenize(stmt);
        let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
        for (clause_tokens, clause_upper) in split_top_level_commas(&tokens, &upper) {
            if clause_upper.windows(2).any(|w| w == ["FOREIGN", "KEY"]) {
                for (i, w) in clause_upper.windows(2).enumerate() {
                    if w[0] == "CONSTRAINT" {
                        fk_constraints.push(unquote_ident(&clause_tokens[i + 1]).to_ascii_lowercase());
                    }
                }
            }
        }
    }
    let mut tables = Vec::new();
    for stmt in statements {
        let tokens = tokenize(stmt);
        let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
        if upper.first().map(String::as_str) == Some("ALTER")
            && upper.get(1).map(String::as_str) == Some("TABLE")
            && let Some(val_pos) = upper.windows(2).position(|w| w == ["VALIDATE", "CONSTRAINT"])
        {
            let cname = tokens.get(val_pos + 2).map(|c| unquote_ident(c).to_ascii_lowercase()).unwrap_or_default();
            if !cname.is_empty() && fk_constraints.contains(&cname) {
                continue;
            }
            let mut pos = 2;
            if upper.get(pos).map(String::as_str) == Some("IF")
                && upper.get(pos + 1).map(String::as_str) == Some("EXISTS")
            {
                pos += 2;
            }
            if upper.get(pos).map(String::as_str) == Some("ONLY") {
                pos += 1;
            }
            if let Some(raw_table) = tokens.get(pos) {
                tables.push(raw_table.clone());
            }
        }
    }
    tables
}

pub(super) fn check_alter_table(
    snapshot: &Snapshot,
    stmt: &str,
    tokens: &[String],
    ctx: &ScriptContext<'_>,
    findings: &mut Vec<Finding>,
) {
    let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    if upper.first().map(String::as_str) != Some("ALTER") || upper.get(1).map(String::as_str) != Some("TABLE") {
        return;
    }
    let mut pos = 2;
    if upper.get(pos).map(String::as_str) == Some("IF") && upper.get(pos + 1).map(String::as_str) == Some("EXISTS") {
        pos += 2;
    }
    if upper.get(pos).map(String::as_str) == Some("ONLY") {
        pos += 1;
    }
    let Some(raw_table) = tokens.get(pos) else {
        return;
    };
    let Some(table) = find_table(snapshot, raw_table) else {
        return;
    };
    if pos + 1 >= tokens.len() {
        return;
    }

    let before_count = findings.len();
    let scope = AlterTableScope { snapshot, stmt, table, ctx, before_count };
    for (clause_tokens, clause_upper) in split_top_level_commas(&tokens[pos + 1..], &upper[pos + 1..]) {
        check_alter_table_action(&scope, clause_tokens, clause_upper, findings);
    }
    let adds_fk = upper.windows(2).any(|w| w == ["FOREIGN", "KEY"]);
    if snapshot.server.engine == Engine::Mysql
        && is_large_table(table)
        && !has_mysql_online_ddl(&upper)
        && !adds_fk
        && !findings[before_count..].iter().any(|f| {
            f.id.starts_with("sql-lint:table-rewrite:")
                || f.id.starts_with("sql-lint:volatile-default:")
                || f.id.starts_with("sql-lint:not-null-without-default:")
        })
    {
        let rows = table_rows(table);
        let bytes = table_bytes(table);
        let qual = table.qualified();
        let mut f = finding(
            "sql-lint",
            &format!("mysql-online-ddl:{qual}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "ALTER TABLE on {} ({}) without LOCK=NONE or ALGORITHM=INSTANT can lock the table",
            table.name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` alters {} ({}) without specifying `ALGORITHM=INSTANT` or `ALGORITHM=INPLACE, \
             LOCK=NONE`. Without an explicit online DDL clause, MySQL/MariaDB can fall back to `ALGORITHM=COPY` or \
             `LOCK=SHARED`, blocking concurrent writes while rebuilding the table.",
            compact_sql(stmt),
            qual,
            table_size_label(rows, bytes)
        );
        let can_append = !upper.iter().any(|t| matches!(t.as_str(), "ALGORITHM" | "LOCK"));
        f.fix = Some(Fix {
            summary: "Append `ALGORITHM=INPLACE, LOCK=NONE` (or `ALGORITHM=INSTANT` for compatible column additions on MySQL 8.0+) so the database rejects any blocking table copy immediately.".into(),
            sql: can_append.then(|| append_sql_clause(stmt, ", ALGORITHM=INPLACE, LOCK=NONE;")),
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec![
            "Re-run `culprit lint-sql` after adding `ALGORITHM=INPLACE, LOCK=NONE` or `ALGORITHM=INSTANT`.".into(),
        ];
        findings.push(f);
    }
}

struct AlterTableScope<'a> {
    snapshot: &'a Snapshot,
    stmt: &'a str,
    table: &'a TableStat,
    ctx: &'a ScriptContext<'a>,
    before_count: usize,
}

fn check_alter_table_action(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    check_foreign_key_clause(scope, tokens, upper, findings);
    check_drop_or_rename_column(scope, tokens, upper, findings);
    check_add_not_null_without_default(scope, tokens, upper, findings);

    if !is_large_table(scope.table) {
        return;
    }

    check_postgres_constraint_additions(scope, tokens, upper, findings);
    check_add_volatile_default(scope, tokens, upper, findings);
    check_column_type_or_set_not_null(scope, tokens, upper, findings);
}

fn check_foreign_key_clause(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    if let Some(fk_pos) = upper.windows(2).position(|w| w == ["FOREIGN", "KEY"])
        && let Some((child_cols, rest_idx)) = parse_paren_list(tokens, fk_pos + 2)
        && let Some(ref_offset) = upper[rest_idx..].iter().position(|t| t == "REFERENCES")
    {
        let ref_pos = rest_idx + ref_offset;
        if let Some(raw_parent) = tokens.get(ref_pos + 1) {
            let parent_cols = parse_paren_list(tokens, ref_pos + 2).map(|(cols, _)| cols).unwrap_or_default();
            check_foreign_key_addition(scope, &child_cols, raw_parent, &parent_cols, findings);
        }
    }
}

fn check_drop_or_rename_column(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    if upper.first().map(String::as_str) == Some("DROP") {
        let second = upper.get(1).map(String::as_str).unwrap_or("");
        if !matches!(second, "CONSTRAINT" | "FOREIGN" | "CHECK" | "PRIMARY" | "UNIQUE" | "INDEX") {
            let mut col_idx = if second == "COLUMN" { 2 } else { 1 };
            if upper.get(col_idx).map(String::as_str) == Some("IF")
                && upper.get(col_idx + 1).map(String::as_str) == Some("EXISTS")
            {
                col_idx += 2;
            }
            if let Some(raw_col) = tokens.get(col_idx) {
                let col_name = unquote_ident(raw_col);
                check_workload_references(
                    scope.snapshot,
                    scope.stmt,
                    scope.table,
                    Some(&col_name),
                    "DROP COLUMN",
                    findings,
                );
            }
        }
    } else if upper.first().map(String::as_str) == Some("RENAME") {
        let second = upper.get(1).map(String::as_str).unwrap_or("");
        if second == "COLUMN" {
            if let Some(raw_col) = tokens.get(2) {
                let col_name = unquote_ident(raw_col);
                check_workload_references(
                    scope.snapshot,
                    scope.stmt,
                    scope.table,
                    Some(&col_name),
                    "RENAME COLUMN",
                    findings,
                );
            }
        } else if second == "TO" {
            check_workload_references(scope.snapshot, scope.stmt, scope.table, None, "RENAME TABLE", findings);
        } else if !matches!(second, "CONSTRAINT" | "INDEX" | "KEY" | "")
            && upper.get(2).map(String::as_str) == Some("TO")
            && let Some(raw_col) = tokens.get(1)
        {
            let col_name = unquote_ident(raw_col);
            check_workload_references(
                scope.snapshot,
                scope.stmt,
                scope.table,
                Some(&col_name),
                "RENAME COLUMN",
                findings,
            );
        }
    } else if upper.first().map(String::as_str) == Some("CHANGE") {
        let col_idx = if upper.get(1).map(String::as_str) == Some("COLUMN") { 2 } else { 1 };
        if let (Some(raw_old), Some(raw_new)) = (tokens.get(col_idx), tokens.get(col_idx + 1)) {
            let old_col = unquote_ident(raw_old);
            let new_col = unquote_ident(raw_new);
            if !old_col.eq_ignore_ascii_case(&new_col) {
                check_workload_references(
                    scope.snapshot,
                    scope.stmt,
                    scope.table,
                    Some(&old_col),
                    "RENAME COLUMN",
                    findings,
                );
            }
        }
    }
}

fn check_add_not_null_without_default(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    let table = scope.table;
    if upper.first().map(String::as_str) != Some("ADD") || table_rows(table) < 1.0 {
        return;
    }
    let second = upper.get(1).map(String::as_str).unwrap_or("");
    if matches!(
        second,
        "CONSTRAINT"
            | "FOREIGN"
            | "CHECK"
            | "PRIMARY"
            | "UNIQUE"
            | "EXCLUDE"
            | "INDEX"
            | "KEY"
            | "FULLTEXT"
            | "SPATIAL"
    ) {
        return;
    }
    let mut col_idx = if second == "COLUMN" { 2 } else { 1 };
    if upper.get(col_idx).map(String::as_str) == Some("IF")
        && upper.get(col_idx + 1).map(String::as_str) == Some("NOT")
        && upper.get(col_idx + 2).map(String::as_str) == Some("EXISTS")
    {
        col_idx += 3;
    }
    let Some(raw_col) = tokens.get(col_idx) else {
        return;
    };
    let rest = &upper[col_idx + 1..];
    let has_not_null = has_top_level_not_null(rest);
    let has_default = rest.iter().any(|t| {
        matches!(
            t.as_str(),
            "DEFAULT"
                | "GENERATED"
                | "IDENTITY"
                | "SERIAL"
                | "BIGSERIAL"
                | "SMALLSERIAL"
                | "SERIAL4"
                | "SERIAL8"
                | "SERIAL2"
                | "AUTO_INCREMENT"
        )
    });
    if has_not_null && !has_default {
        let rows = table_rows(table);
        let bytes = table_bytes(table);
        let qual = table.qualified();
        let col_name = unquote_ident(raw_col);
        let mut f = finding(
            "sql-lint",
            &format!("not-null-without-default:{qual}.{col_name}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = Severity::High;
        f.impact.score = base_score(f.severity);
        f.impact.label = "fails on existing rows".into();
        f.title = format!(
            "ADD COLUMN {}.{} NOT NULL without DEFAULT fails on non-empty table ({})",
            table.name,
            col_name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` adds `{col_name}` as `NOT NULL` without a `DEFAULT` clause to {} ({}). \
             Because existing rows would receive `NULL` for `{col_name}`, the database rejects the \
             statement immediately at runtime.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes)
        );
        f.fix = Some(Fix {
            summary: format!(
                "Either supply a non-volatile `DEFAULT` value, or add `{col_name}` as nullable first, backfill existing rows, and then enforce `NOT NULL`."
            ),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify =
            vec!["Re-run `culprit lint-sql` after adding a `DEFAULT` or making the initial column nullable.".into()];
        findings.push(f);
    }
}

fn check_postgres_constraint_additions(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    if scope.snapshot.server.engine != Engine::Postgres || upper.first().map(String::as_str) != Some("ADD") {
        return;
    }
    let table = scope.table;
    let rows = table_rows(table);
    let bytes = table_bytes(table);
    let qual = table.qualified();
    let is_fk = upper.windows(2).any(|w| w == ["FOREIGN", "KEY"]);
    let is_check = upper.iter().any(|t| t == "CHECK");
    let has_not_valid = upper.windows(2).any(|w| w == ["NOT", "VALID"]);
    let is_unique = upper.iter().any(|t| t == "UNIQUE");
    let is_pk = upper.windows(2).any(|w| w == ["PRIMARY", "KEY"]);
    let has_using_index = upper.windows(2).any(|w| w == ["USING", "INDEX"]);
    if (is_fk || is_check) && !has_not_valid {
        let kind = if is_fk { "FOREIGN KEY" } else { "CHECK" };
        let cname = if upper.get(1).map(String::as_str) == Some("CONSTRAINT") {
            tokens.get(2).map(|s| unquote_ident(s)).unwrap_or_else(|| kind.to_ascii_lowercase().replace(' ', "-"))
        } else {
            kind.to_ascii_lowercase().replace(' ', "-")
        };
        let mut f = finding(
            "sql-lint",
            &format!("unvalidated-constraint:{qual}:{cname}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "ADD {kind} constraint on {} ({}) without NOT VALID scans the table under a lock",
            table.name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` adds a {kind} constraint on {} ({}) without `NOT VALID`. PostgreSQL validates \
             every existing row immediately while holding a `SHARE ROW EXCLUSIVE` / `ACCESS EXCLUSIVE` lock that \
             blocks writes on {}.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes),
            table.name
        );
        f.fix = Some(Fix {
            summary: format!(
                "Add the {kind} constraint with `NOT VALID` first, then run `ALTER TABLE {qual} VALIDATE CONSTRAINT {cname};` in a separate step (which only holds `SHARE UPDATE EXCLUSIVE` and allows concurrent reads and writes)."
            ),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec!["Re-run `culprit lint-sql` after adding `NOT VALID` to the constraint definition.".into()];
        findings.push(f);
    } else if (is_unique || is_pk) && !has_using_index {
        let kind = if is_pk { "PRIMARY KEY" } else { "UNIQUE" };
        let cname = if upper.get(1).map(String::as_str) == Some("CONSTRAINT") {
            tokens.get(2).map(|s| unquote_ident(s)).unwrap_or_else(|| kind.to_ascii_lowercase().replace(' ', "-"))
        } else {
            kind.to_ascii_lowercase().replace(' ', "-")
        };
        let mut f = finding(
            "sql-lint",
            &format!("blocking-unique-constraint:{qual}:{cname}"),
            Category::Indexes,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "ADD {kind} constraint on {} ({}) without USING INDEX builds the index under an ACCESS EXCLUSIVE lock",
            table.name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` adds a {kind} constraint on {} ({}) directly. PostgreSQL builds the backing unique \
             index while holding an `ACCESS EXCLUSIVE` lock on {}, blocking all reads and writes until the index \
             build finishes.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes),
            table.name
        );
        f.fix = Some(Fix {
            summary: format!(
                "Build the unique index first with `CREATE UNIQUE INDEX CONCURRENTLY`, then attach it with `ALTER TABLE {qual} ADD CONSTRAINT {cname} {kind} USING INDEX <index_name>;`."
            ),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec![
            "Re-run `culprit lint-sql` after switching to `CREATE UNIQUE INDEX CONCURRENTLY` + `ADD CONSTRAINT ... USING INDEX`.".into(),
        ];
        findings.push(f);
    }
}

fn check_add_volatile_default(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    if upper.first().map(String::as_str) != Some("ADD") {
        return;
    }
    let second = upper.get(1).map(String::as_str).unwrap_or("");
    if matches!(second, "CONSTRAINT" | "FOREIGN" | "CHECK" | "PRIMARY" | "UNIQUE" | "EXCLUDE") {
        return;
    }
    let mut col_idx = if second == "COLUMN" { 2 } else { 1 };
    if upper.get(col_idx).map(String::as_str) == Some("IF")
        && upper.get(col_idx + 1).map(String::as_str) == Some("NOT")
        && upper.get(col_idx + 2).map(String::as_str) == Some("EXISTS")
    {
        col_idx += 3;
    }
    let (Some(raw_col), Some(def_pos)) =
        (tokens.get(col_idx), upper[col_idx + 1..].iter().position(|t| t == "DEFAULT"))
    else {
        return;
    };
    let fn_idx = col_idx + 1 + def_pos + 1;
    if let Some(fn_tok) = upper.get(fn_idx)
        && tokens.get(fn_idx + 1).map(String::as_str) == Some("(")
        && is_volatile_default_function(fn_tok)
    {
        let table = scope.table;
        let rows = table_rows(table);
        let bytes = table_bytes(table);
        let qual = table.qualified();
        let col_name = unquote_ident(raw_col);
        let fn_display = tokens[fn_idx].clone();
        let mut f = finding(
            "sql-lint",
            &format!("volatile-default:{qual}.{col_name}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "ADD COLUMN {}.{} with volatile DEFAULT {}() rewrites the table ({})",
            table.name,
            col_name,
            fn_display,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` adds `{col_name}` to {} ({}) with volatile `DEFAULT {fn_display}()`, which \
             cannot use metadata-only fast defaults because every existing row requires a newly evaluated \
             value. The engine rewrites every row and index while holding an exclusive table lock.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes)
        );
        f.fix = Some(Fix {
            summary: format!(
                "Add `{col_name}` without a default, backfill existing rows in batches, and then set `ALTER TABLE {qual} ALTER COLUMN {col_name} SET DEFAULT {fn_display}();`."
            ),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec!["Verify in staging with `SET lock_timeout = '2s'` before running on production.".into()];
        findings.push(f);
    }
}

fn has_top_level_not_null(upper: &[String]) -> bool {
    let mut depth = 0usize;
    for (i, tok) in upper.iter().enumerate() {
        match tok.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "NOT"
                if depth == 0
                    && upper.get(i + 1).map(String::as_str) == Some("NULL")
                    && (i == 0 || upper[i - 1] != "IS") =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn check_column_type_or_set_not_null(
    scope: &AlterTableScope<'_>,
    tokens: &[String],
    upper: &[String],
    findings: &mut Vec<Finding>,
) {
    if !matches!(upper.first().map(String::as_str), Some("ALTER" | "MODIFY" | "CHANGE")) {
        return;
    }
    let idx = 0;
    let table = scope.table;
    let rows = table_rows(table);
    let bytes = table_bytes(table);
    let qual = table.qualified();
    let has_set_not_null = upper[idx..].windows(3).any(|w| w == ["SET", "NOT", "NULL"]);
    let table_validated = scope.ctx.validated_tables.iter().any(|raw| table_matches(table, raw));
    let type_change = parse_column_type_change(tokens, upper, idx);
    let is_rewrite = type_change.is_some_and(|(col_name, new_type, has_using_or_collate)| {
        if has_using_or_collate {
            return true;
        }
        let existing = scope.snapshot.columns.iter().find(|c| {
            c.schema.eq_ignore_ascii_case(&table.schema)
                && c.table.eq_ignore_ascii_case(&table.name)
                && c.name.eq_ignore_ascii_case(&col_name)
        });
        match existing {
            Some(col) => !is_metadata_only_type_change(scope.snapshot.server.engine, &col.data_type, &new_type),
            None => true,
        }
    });
    if is_rewrite && !findings[scope.before_count..].iter().any(|f| f.id == format!("sql-lint:table-rewrite:{qual}")) {
        let mut f = finding(
            "sql-lint",
            &format!("table-rewrite:{qual}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if rows >= HIGH_TABLE_ROWS || bytes >= GB { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "Column type change on {} ({}) can rewrite the table under an exclusive lock",
            table.name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` alters a column type on {} ({}). Changing a column's representation rewrites \
             every row and index while holding an `ACCESS EXCLUSIVE` / schema-modification lock, blocking all \
             reads and writes on {}.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes),
            table.name
        );
        f.fix = Some(Fix {
            summary: "Prefer an expand-and-contract migration (add a new column, backfill in small batches, swap reads/writes) unless the engine guarantees a metadata-only cast.".into(),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec!["Verify in staging with `SET lock_timeout = '2s'` before running on production.".into()];
        findings.push(f);
    } else if has_set_not_null
        && scope.snapshot.server.engine == Engine::Postgres
        && !table_validated
        && !findings[scope.before_count..].iter().any(|f| f.id == format!("sql-lint:set-not-null:{qual}"))
    {
        let mut f = finding(
            "sql-lint",
            &format!("set-not-null:{qual}"),
            Category::Maintenance,
            subject(SubjectKind::Table, &qual),
        );
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = Severity::Medium;
        f.impact.score = base_score(f.severity);
        f.impact.label = table_size_label(rows, bytes);
        f.title = format!(
            "SET NOT NULL on {} ({}) scans the table under an ACCESS EXCLUSIVE lock unless backed by a valid constraint",
            table.name,
            table_size_label(rows, bytes)
        );
        f.summary = format!(
            "The statement `{}` runs `SET NOT NULL` on {} ({}). Without an existing validated `CHECK (col IS NOT \
             NULL)` constraint, PostgreSQL scans the entire table while holding an `ACCESS EXCLUSIVE` lock.",
            compact_sql(scope.stmt),
            qual,
            table_size_label(rows, bytes)
        );
        f.fix = Some(Fix {
            summary: "Add `CHECK (col IS NOT NULL) NOT VALID`, run `ALTER TABLE ... VALIDATE CONSTRAINT ...` (which only takes `SHARE UPDATE EXCLUSIVE`), and then run `SET NOT NULL`.".into(),
            sql: None,
            cost: None,
        });
        push_table_metrics(&mut f, rows, bytes);
        f.verify = vec!["Re-run `culprit lint-sql` after splitting the constraint validation step.".into()];
        findings.push(f);
    }
}

fn is_volatile_default_function(upper_fn: &str) -> bool {
    let bare = upper_fn.rsplit('.').next().unwrap_or(upper_fn);
    matches!(
        bare,
        "GEN_RANDOM_UUID"
            | "UUID_GENERATE_V4"
            | "UUID_GENERATE_V1"
            | "UUID_GENERATE_V1MC"
            | "RANDOM"
            | "CLOCK_TIMESTAMP"
            | "TIMEOFDAY"
            | "NEWID"
            | "NEWSEQUENTIALID"
            | "UUID"
            | "RAND"
    )
}

fn check_foreign_key_addition(
    scope: &AlterTableScope<'_>,
    child_cols: &[String],
    raw_parent: &str,
    parent_cols: &[String],
    findings: &mut Vec<Finding>,
) {
    let snapshot = scope.snapshot;
    let stmt = scope.stmt;
    let child_table = scope.table;
    let ctx = scope.ctx;
    let before_count = scope.before_count;
    if child_cols.is_empty() {
        return;
    }
    let child_qual = child_table.qualified();
    let covered_in_snapshot = snapshot.indexes.iter().any(|ix| {
        ix.schema.eq_ignore_ascii_case(&child_table.schema)
            && ix.table.eq_ignore_ascii_case(&child_table.name)
            && !ix.is_disabled
            && index_covers_prefix(&ix.columns, child_cols)
    });
    let covered_in_script = ctx
        .created_indexes
        .iter()
        .any(|(tbl, cols)| table_matches(child_table, tbl) && index_covers_prefix(cols, child_cols));
    let cols_label = child_cols.join(", ");
    let fk_finding_id = format!("unindexed-fk:{child_qual}.{cols_label}");
    if !covered_in_snapshot
        && !covered_in_script
        && !findings[before_count..].iter().any(|f| f.id == format!("sql-lint:{fk_finding_id}"))
    {
        let mut f = finding("sql-lint", &fk_finding_id, Category::Indexes, subject(SubjectKind::Table, &child_qual));
        f.actionability = Actionability::NeedsAttention;
        f.confidence = Confidence::High;
        f.severity = if is_large_table(child_table) { Severity::High } else { Severity::Medium };
        f.impact.score = base_score(f.severity);
        f.impact.label = "unindexed FK".into();
        f.title = format!("New foreign key on {}.({}) has no supporting index", child_table.name, cols_label);
        f.summary = format!(
            "The statement `{}` adds a foreign key on {}.({}) referencing {}, but neither the snapshot nor the migration \
             defines an index with ({}) as its leading key. Deletes or key updates on the referenced parent table \
             will scan {} on every check.",
            compact_sql(stmt),
            child_qual,
            cols_label,
            raw_parent,
            cols_label,
            child_table.name
        );
        let pg_in_tx = ctx.in_transaction && snapshot.server.engine == Engine::Postgres;
        f.fix = Some(Fix {
            summary: format!("Create an index on {child_qual} ({cols_label}) before or alongside the foreign key."),
            sql: (!pg_in_tx).then(|| {
                crate::support::create_index_sql(snapshot, &child_table.schema, &child_table.name, child_cols, &[])
            }),
            cost: None,
        });
        f.verify = vec!["Add the supporting `CREATE INDEX` to the migration and re-run `culprit lint-sql`.".into()];
        findings.push(f);
    }

    if let Some(parent_table) = find_table(snapshot, raw_parent) {
        for (child_col, parent_col) in child_cols.iter().zip(parent_cols.iter()) {
            let c_info = snapshot.columns.iter().find(|c| {
                c.schema.eq_ignore_ascii_case(&child_table.schema)
                    && c.table.eq_ignore_ascii_case(&child_table.name)
                    && c.name.eq_ignore_ascii_case(child_col)
            });
            let p_info = snapshot.columns.iter().find(|c| {
                c.schema.eq_ignore_ascii_case(&parent_table.schema)
                    && c.table.eq_ignore_ascii_case(&parent_table.name)
                    && c.name.eq_ignore_ascii_case(parent_col)
            });
            if let (Some(c), Some(p)) = (c_info, p_info)
                && normalized_base_type(&c.data_type) != normalized_base_type(&p.data_type)
            {
                let mut f = finding(
                    "sql-lint",
                    &format!("fk-type-mismatch:{child_qual}.{}", c.name),
                    Category::Indexes,
                    subject(SubjectKind::Table, &child_qual),
                );
                f.actionability = Actionability::NeedsAttention;
                f.confidence = Confidence::High;
                f.severity = Severity::Medium;
                f.impact.score = base_score(f.severity);
                f.impact.label = format!("{} vs {}", c.data_type, p.data_type);
                f.title = format!(
                    "Foreign key {}.{} ({}) does not match {}.{} ({})",
                    child_table.name, c.name, c.data_type, parent_table.name, p.name, p.data_type
                );
                f.summary = format!(
                    "The statement `{}` references {}.{} (`{}`) from {}.{} (`{}`). Mismatched foreign key types either \
                     fail DDL validation or force implicit conversions during joins and referential integrity checks.",
                    compact_sql(stmt),
                    parent_table.qualified(),
                    p.name,
                    p.data_type,
                    child_qual,
                    c.name,
                    c.data_type
                );
                f.fix = Some(Fix {
                    summary: format!(
                        "Align {}.{}'s type with {}.{} (`{}`).",
                        child_qual,
                        c.name,
                        parent_table.qualified(),
                        p.name,
                        p.data_type
                    ),
                    sql: None,
                    cost: None,
                });
                f.verify = vec!["Confirm both columns share the same data type before adding the constraint.".into()];
                findings.push(f);
            }
        }
    }
}

fn index_covers_prefix(index_cols: &[String], fk_cols: &[String]) -> bool {
    if index_cols.len() < fk_cols.len() {
        return false;
    }
    let prefix = &index_cols[..fk_cols.len()];
    fk_cols.iter().all(|fk| {
        let fk_clean = culprit_core::strip_direction(fk);
        prefix.iter().any(|ic| culprit_core::strip_direction(ic).eq_ignore_ascii_case(fk_clean))
    })
}

pub(super) fn normalized_base_type(ty: &str) -> String {
    let lower = ty.trim().to_ascii_lowercase();
    let is_unsigned = lower.split_whitespace().any(|w| w == "unsigned" || w.ends_with(")unsigned"));
    let base_raw = lower
        .split('(')
        .next()
        .unwrap_or(&lower)
        .split_whitespace()
        .filter(|w| *w != "unsigned" && *w != "signed")
        .collect::<Vec<_>>()
        .join(" ");
    let canonical = match base_raw.as_str() {
        "int4" | "serial" | "int" => "integer",
        "int8" | "bigserial" => "bigint",
        "int2" | "smallserial" => "smallint",
        "character varying" => "varchar",
        other => other,
    };
    if is_unsigned { format!("{canonical} unsigned") } else { canonical.to_string() }
}

/// Parse an `ALTER [COLUMN] <col> [SET DATA] TYPE <new_type>`, `MODIFY [COLUMN] <col> <new_type>`,
/// `CHANGE [COLUMN] <old_col> <new_col> <new_type>`, or SQL Server `ALTER COLUMN <col> <new_type>` clause into
/// `(column_name, new_type_spec, has_using_or_collate)`.
fn parse_column_type_change(tokens: &[String], upper: &[String], idx: usize) -> Option<(String, String, bool)> {
    let head = upper.get(idx)?.as_str();
    let mut pos = idx + 1;
    let had_column_kw = upper.get(pos).map(String::as_str) == Some("COLUMN");
    if had_column_kw {
        pos += 1;
    }
    let col_name = unquote_ident(tokens.get(pos)?);
    pos += 1;
    let type_start = if head == "MODIFY" {
        pos
    } else if head == "CHANGE" {
        // MySQL: CHANGE [COLUMN] <old_col> <new_col> <new_type>
        pos + 1
    } else if let Some(type_offset) = upper[pos..].iter().position(|t| t == "TYPE") {
        let between = &upper[pos..pos + type_offset];
        if !between.is_empty() && between != ["SET", "DATA"] {
            return None;
        }
        pos + type_offset + 1
    } else if head == "ALTER"
        && had_column_kw
        && let Some(next_kw) = upper.get(pos).map(String::as_str)
        && !matches!(next_kw, "SET" | "DROP" | "RESET" | "ADD" | "TYPE" | "RESTART")
    {
        pos
    } else {
        return None;
    };
    if type_start >= tokens.len() {
        return None;
    }
    let has_using_or_collate = upper[type_start..].iter().any(|t| matches!(t.as_str(), "USING" | "COLLATE"));
    let type_end = upper[type_start..]
        .iter()
        .position(|t| {
            matches!(
                t.as_str(),
                "USING"
                    | "COLLATE"
                    | "NULL"
                    | "NOT"
                    | "DEFAULT"
                    | "COMMENT"
                    | "FIRST"
                    | "AFTER"
                    | "GENERATED"
                    | "IDENTITY"
            )
        })
        .map_or(tokens.len(), |off| type_start + off);
    if type_start >= type_end {
        return None;
    }
    let mut new_type = String::new();
    for tok in &tokens[type_start..type_end] {
        if matches!(tok.as_str(), "(" | ")" | ",") {
            new_type.push_str(tok);
        } else {
            if !new_type.is_empty() && !new_type.ends_with(['(', ',']) {
                new_type.push(' ');
            }
            new_type.push_str(tok);
        }
    }
    Some((col_name, new_type, has_using_or_collate))
}

fn parse_type_spec(raw: &str) -> (String, Option<(u32, Option<u32>)>) {
    let lower = raw.trim().to_ascii_lowercase();
    let (base_part, params_part) = match lower.split_once('(') {
        Some((b, p)) => (b, p.split(')').next()),
        None => (lower.as_str(), None),
    };
    let base_collapsed = base_part.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = match base_collapsed.as_str() {
        "character varying" | "varchar" => "varchar".to_string(),
        "national character varying" | "nvarchar" => "nvarchar".to_string(),
        "decimal" | "numeric" => "numeric".to_string(),
        "bit varying" | "varbit" => "varbit".to_string(),
        other => other.to_string(),
    };
    let params = params_part.and_then(|p| {
        let mut parts = p.split(',');
        let p1 = parts.next()?.trim().parse::<u32>().ok()?;
        let p2 = parts.next().and_then(|s| s.trim().parse::<u32>().ok());
        Some((p1, p2))
    });
    (base, params)
}

fn is_metadata_only_type_change(engine: Engine, old_type: &str, new_type: &str) -> bool {
    let (old_base, old_params) = parse_type_spec(old_type);
    let (new_base, new_params) = parse_type_spec(new_type);
    match engine {
        Engine::Postgres | Engine::Yugabyte => match (old_base.as_str(), new_base.as_str()) {
            ("varchar", "varchar") => match (old_params, new_params) {
                (Some((old_len, None)), Some((new_len, None))) => new_len >= old_len,
                (_, None) => true,
                _ => false,
            },
            ("varchar" | "text", "text") => new_params.is_none(),
            ("numeric", "numeric") => match (old_params, new_params) {
                (Some((old_p, old_s)), Some((new_p, new_s))) => {
                    new_p >= old_p && old_s.unwrap_or(0) == new_s.unwrap_or(0)
                }
                (_, None) => true,
                _ => false,
            },
            ("varbit", "varbit") => match (old_params, new_params) {
                (Some((old_len, None)), Some((new_len, None))) => new_len >= old_len,
                (_, None) => true,
                _ => false,
            },
            _ => false,
        },
        Engine::Mssql => match (old_base.as_str(), new_base.as_str()) {
            ("varchar", "varchar") => matches!(
                (old_params, new_params),
                (Some((old_len, None)), Some((new_len, None))) if new_len >= old_len && new_len <= 8000
            ),
            ("nvarchar", "nvarchar") => matches!(
                (old_params, new_params),
                (Some((old_len, None)), Some((new_len, None))) if new_len >= old_len && new_len <= 4000
            ),
            _ => false,
        },
        Engine::Mysql | Engine::Sqlite => false,
    }
}
