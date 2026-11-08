//! The `unindexed-fk` check: Foreign key columns with no index starting with them, which makes deletes on the parent and joins from the child slow.

use super::*;
use crate::evidence::Requirement as Evidence;
use culprit_core::{ForeignKey, t};

pub fn unindexed_fk_info() -> CheckInfo {
    CheckInfo {
        id: "unindexed-fk".into(),
        title: t!("check.unindexed_fk.info.title"),
        category: Category::Indexes,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte, Engine::Mysql, Engine::Sqlite],
        requires: vec![Capability::TableStats],
        description: t!("check.unindexed_fk.info.description"),
    }
}

check!(
    UnindexedFkCheck: unindexed_fk_info,
    decide = unindexed_fk_cases,
    render = unindexed_fk_finding,
    case<'s> = UnindexedFk<'s>,
    evidence = &[Evidence::IndexCatalog, Evidence::ForeignKeys],
);

/// The unindexed-fk findings of `snapshot`: its cases, each worded.
#[cfg(test)]
pub(crate) fn unindexed_fk(snapshot: &Snapshot) -> Vec<Finding> {
    culprit_core::Check::run(&UnindexedFkCheck, snapshot)
}

/// What the unindexed-fk check concludes about one subject.
pub enum UnindexedFk<'s> {
    /// A foreign key on a table large enough for a finding of its own, that no index serves.
    Key(UnindexedKey<'s>),
    /// Many foreign keys without an index, mostly on small tables: the habit, not the table.
    Pattern(UnindexedPattern<'s>),
    /// A foreign key whose column's type differs from its parent's.
    TypeMismatch(TypeMismatch<'s>),
    /// An indexed or key column that stores UUIDs as text.
    UuidText(UuidText<'s>),
}

fn unindexed_fk_cases(snapshot: &Snapshot) -> Vec<UnindexedFk<'_>> {
    let unindexed = unindexed_foreign_keys(snapshot);
    let mut out: Vec<UnindexedFk> = unindexed
        .iter()
        .filter_map(|(fk, rows)| {
            rows.filter(|r| *r >= FK_OWN_FINDING_ROWS).map(|r| UnindexedFk::Key(unindexed_key(snapshot, fk, r)))
        })
        .collect();
    out.extend(unindexed_fk_pattern(snapshot, &unindexed).map(UnindexedFk::Pattern));
    out.extend(fk_type_mismatches(snapshot).into_iter().map(UnindexedFk::TypeMismatch));
    out.extend(uuid_text_indexed_columns(snapshot).into_iter().map(UnindexedFk::UuidText));
    out
}

fn unindexed_fk_finding(snapshot: &Snapshot, case: UnindexedFk<'_>) -> Finding {
    match case {
        UnindexedFk::Key(k) => unindexed_key_finding(snapshot, k),
        UnindexedFk::Pattern(p) => pattern_finding(snapshot, p),
        UnindexedFk::TypeMismatch(m) => type_mismatch_finding(m),
        UnindexedFk::UuidText(u) => uuid_text_finding(u),
    }
}

/// Whether an index can serve the foreign key: it leads with the key columns, isn't disabled, and covers every row the
/// check can match. A filtered (partial) index can't, unless its filter only leaves out rows whose key is NULL (those
/// never match a parent row): checking a parent delete has to find soft-deleted children too.
pub(crate) fn fk_is_indexed(snapshot: &Snapshot, fk: &culprit_core::ForeignKey) -> bool {
    snapshot.indexes_on(&fk.schema, &fk.table).any(|i| {
        !i.is_disabled
            && leads_with(i, &fk.columns)
            && i.filter.as_deref().is_none_or(|f| only_excludes_null_keys(f, &fk.columns))
    })
}

pub(super) fn leads_with(i: &IndexStat, columns: &[String]) -> bool {
    let keys = i.key_names();
    keys.len() >= columns.len()
        && i.can_seek(columns)
        && lower_set(&keys[..columns.len()].iter().map(|s| s.to_string()).collect::<Vec<_>>()) == lower_set(columns)
}

/// `([customer_id] IS NOT NULL)`, `(a IS NOT NULL AND b IS NOT NULL)`: a filter whose every condition is one of the key
/// columns being NOT NULL.
pub(super) fn only_excludes_null_keys(filter: &str, columns: &[String]) -> bool {
    let plain: String = filter
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '"' | '`' | '(' | ')'))
        .collect::<String>()
        .to_ascii_lowercase();
    let words: Vec<&str> = plain.split_whitespace().collect();
    words.split(|w| *w == "and").all(|condition| match condition {
        [column, "is", "not", "null"] => {
            let name = column.rsplit('.').next().unwrap_or(column);
            columns.iter().any(|c| c.eq_ignore_ascii_case(name))
        }
        _ => false,
    })
}

/// Child tables smaller than this get no finding of their own: reading one in full is cheap today.
pub(crate) const FK_OWN_FINDING_ROWS: f64 = 10_000.0;

/// A child table this big, read in full for each parent row deleted this many times a day, is Medium: about half a
/// second a read of a million rows makes it a minute a day, Medium's amount of time.
const FK_MEDIUM_ROWS: f64 = 1_000_000.0;
const FK_MEDIUM_PARENT_DELETES_A_DAY: f64 = 100.0;

/// An index led by the key's columns that its filter keeps from serving the key.
pub(super) fn filtered_fk_index<'a>(snapshot: &'a Snapshot, fk: &'a culprit_core::ForeignKey) -> Option<&'a IndexStat> {
    snapshot.indexes_on(&fk.schema, &fk.table).find(|i| {
        leads_with(i, &fk.columns) && i.filter.as_deref().is_some_and(|f| !only_excludes_null_keys(f, &fk.columns))
    })
}

/// Whether the database checks the key when a parent row is deleted or re-keyed. SQL Server's NOCHECK CONSTRAINT
/// switches a foreign key off entirely (`sys.foreign_keys.is_disabled`, read with the untrusted constraints): nothing
/// looks for child rows then, so nothing needs an index for it. A key that is only untrusted (added WITH NOCHECK) is
/// still checked on every delete.
pub(crate) fn fk_is_enforced(snapshot: &Snapshot, fk: &culprit_core::ForeignKey) -> bool {
    !snapshot.constraints.iter().any(|c| {
        c.kind == culprit_core::ConstraintKind::ForeignKey
            && c.disabled
            && c.name.eq_ignore_ascii_case(&fk.name)
            && c.schema.eq_ignore_ascii_case(&fk.schema)
            && c.table.eq_ignore_ascii_case(&fk.table)
    })
}

/// The foreign keys the database checks (see [`fk_is_enforced`]) that have columns.
pub(crate) fn enforced_foreign_keys(snapshot: &Snapshot) -> impl Iterator<Item = &culprit_core::ForeignKey> {
    snapshot.foreign_keys.iter().filter(|fk| !fk.columns.is_empty() && fk_is_enforced(snapshot, fk))
}

/// Every foreign key no index can serve, with its child table's rows when known, largest first. A key repeated on the
/// same columns (a duplicate constraint), or whose columns lead another unindexed key on the same table (one index on
/// the longer key serves both), is listed once. A key the database doesn't check isn't listed.
/// Whether `q`'s captured plan shows no index serving `table` by `columns`: a step that reads the table in full looking
/// for rows by the key (its filter names a key column and keeps few of the rows it reads), or an index the engine
/// builds on those columns for itself on every run. The engine's own account, never the text alone; a statement without
/// a captured plan shows nothing. A full read that filters on something else, or on nothing (a report over every row,
/// the build side of a hash join), wouldn't use an index on the key. On SQLite, whose plans name no conditions, a
/// search through another index that isn't unique, by something other than the key, counts too when the statement
/// compares the key with a value: it reads every row that index finds and checks the key on each (`customer_id = ? AND
/// status = ?` searching an index on status).
pub(crate) fn plan_reads_whole(snapshot: &Snapshot, q: &QueryStat, table: &str, columns: &[String]) -> bool {
    let Some(plan) = &q.plan else { return false };
    let leaf = |name: &str| {
        name.rsplit('.')
            .next()
            .unwrap_or(name)
            .trim_matches(|c| matches!(c, '[' | ']' | '"' | '`'))
            .to_ascii_lowercase()
    };
    let table = leaf(table);
    // A key column as a whole word in `text`, and with `valued`, compared with a value (`customer_id" = ?`), not with
    // another column (a join's ON).
    let names = |text: &str, valued: bool| {
        let text = text.to_ascii_lowercase();
        columns.iter().any(|c| {
            let c = c.to_ascii_lowercase();
            text.match_indices(&c).any(|(at, _)| {
                let word = |ch: Option<char>| ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_');
                let rest = &text[at + c.len()..];
                if word(text[..at].chars().last()) || word(rest.chars().next()) {
                    return false;
                }
                !valued || {
                    let rest = rest.trim_start_matches(['"', '`', ']']).trim_start();
                    rest.strip_prefix('=').is_some_and(|v| {
                        v.trim_start()
                            .starts_with(|ch: char| matches!(ch, '?' | '$' | '\'' | ':' | '@') || ch.is_ascii_digit())
                    }) || rest.starts_with("in (")
                }
            })
        })
    };
    // SQLite's plans name no conditions: there the statement says what it looks the rows up by.
    let sqlite = plan.engine == culprit_core::Engine::Sqlite;
    let by_key = |n: &culprit_core::PlanNode| {
        let names_key = if sqlite {
            names(&q.text, true)
        } else {
            [n.predicate.as_deref(), n.seek_predicate.as_deref()].into_iter().flatten().any(|f| names(f, false))
        };
        names_key && n.kept_share().is_none_or(|k| k <= culprit_core::plan::INDEX_KEEPS_AT_MOST)
    };
    // An index of the table's in the catalog that holds a key value more than once: a search through it can find many
    // rows. The rowid and primary key (SQLite's plan names them so) find one.
    let not_unique = |index: &str| {
        snapshot
            .indexes
            .iter()
            .find(|i| leaf(&i.table) == table && i.name.eq_ignore_ascii_case(index))
            .is_some_and(|i| !i.is_unique && !i.is_primary_key)
    };
    let searches_past_key = |n: &culprit_core::PlanNode| {
        sqlite
            && n.kind == NodeKind::Seek
            && n.index.as_deref().is_some_and(not_unique)
            && !n.seek_predicate.as_deref().is_some_and(|s| names(s, false))
            && names(&q.text, true)
    };
    plan.nodes().iter().any(|n| {
        n.object.as_deref().is_some_and(|o| leaf(o) == table)
            && ((n.kind == NodeKind::Scan && by_key(n)) || searches_past_key(n))
    }) || culprit_core::insights::engine_built_indexes(plan).iter().any(|b| {
        leaf(&b.table) == table
            && b.keys.len() >= columns.len()
            && b.keys.iter().zip(columns).all(|(k, c)| k.eq_ignore_ascii_case(c))
    })
}

pub(crate) fn unindexed_foreign_keys(snapshot: &Snapshot) -> Vec<(&culprit_core::ForeignKey, Option<f64>)> {
    // Absence needs the catalog of the key's own table: a source that failed, or (on YugabyteDB) one of its indexes
    // without sharding metadata, leaves the key unjudged.
    let catalog_known = |fk: &culprit_core::ForeignKey| {
        crate::evidence::index_catalog_for_table(snapshot, &fk.schema, &fk.table).is_none()
    };
    let keys = |fk: &culprit_core::ForeignKey| fk.columns.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>();
    let same_table = |a: &culprit_core::ForeignKey, b: &culprit_core::ForeignKey| {
        a.schema.eq_ignore_ascii_case(&b.schema) && a.table.eq_ignore_ascii_case(&b.table)
    };
    // A foreign key on a partition is the partitioned table's key, copied: judged once, at the parent.
    let is_partition =
        |fk: &culprit_core::ForeignKey| snapshot.table(&fk.schema, &fk.table).is_some_and(|t| t.partition_of.is_some());
    let candidates: Vec<&culprit_core::ForeignKey> = enforced_foreign_keys(snapshot)
        .filter(|fk| !fk_is_indexed(snapshot, fk) && !is_partition(fk) && catalog_known(fk))
        .collect();
    let mut out: Vec<(&culprit_core::ForeignKey, Option<f64>)> = Vec::new();
    for fk in &candidates {
        let (mine, longer) = (keys(fk), |o: &&culprit_core::ForeignKey| {
            let theirs = keys(o);
            same_table(fk, o) && theirs.len() > fk.columns.len() && theirs.starts_with(&keys(fk))
        });
        if candidates.iter().any(longer) || out.iter().any(|(o, _)| same_table(fk, o) && keys(o) == mine) {
            continue;
        }
        out.push((fk, snapshot.table(&fk.schema, &fk.table).and_then(|t| t.rows)));
    }
    out.sort_by(|a, b| {
        b.1.unwrap_or(-1.0)
            .total_cmp(&a.1.unwrap_or(-1.0))
            .then_with(|| (&a.0.schema, &a.0.table, &a.0.name).cmp(&(&b.0.schema, &b.0.table, &b.0.name)))
    });
    out
}

/// Canonical type family for comparing a foreign key's child column against its referenced parent column.
/// Width-only string differences (`varchar(50)` vs `varchar(100)`) and engine aliases (`int4` vs `integer` vs
/// `serial`, `int(11)` vs `int`) map to the same canonical family so only genuine cross-type mismatches (`int` vs
/// `bigint`, `int unsigned` vs `int`, `varchar` vs `nvarchar`, `text` vs `integer`) are reported.
fn canonical_fk_type(engine: Engine, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if engine == Engine::Sqlite {
        let affinity = if lower.contains("int") {
            "integer"
        } else if lower.contains("char") || lower.contains("clob") || lower.contains("text") {
            "text"
        } else if lower.contains("blob") {
            "blob"
        } else if lower.contains("real") || lower.contains("floa") || lower.contains("doub") {
            "real"
        } else {
            "numeric"
        };
        return Some(affinity.into());
    }
    let mut stripped = String::with_capacity(lower.len());
    let mut depth = 0usize;
    for ch in lower.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => stripped.push(ch),
            _ => {}
        }
    }
    let mut unsigned = false;
    let mut collation: Option<String> = None;
    let mut charset: Option<String> = None;
    let raw_words: Vec<&str> = stripped.split_whitespace().collect();
    let mut words = Vec::with_capacity(raw_words.len());
    let mut i = 0;
    while i < raw_words.len() {
        match raw_words[i] {
            "unsigned" => {
                unsigned = true;
                i += 1;
            }
            "signed" | "zerofill" => {
                i += 1;
            }
            "collate" if i + 1 < raw_words.len() => {
                collation = Some(raw_words[i + 1].to_string());
                i += 2;
            }
            "character" if raw_words.get(i + 1) == Some(&"set") && i + 2 < raw_words.len() => {
                charset = Some(raw_words[i + 2].to_string());
                i += 3;
            }
            "charset" if i + 1 < raw_words.len() => {
                charset = Some(raw_words[i + 1].to_string());
                i += 2;
            }
            other => {
                words.push(other);
                i += 1;
            }
        }
    }
    let base = words.join(" ");
    if base.is_empty() {
        return None;
    }
    let family = match base.as_str() {
        "int1" | "tinyint" => "tinyint",
        "int2" | "smallint" | "smallserial" | "serial2" => "smallint",
        "int3" | "mediumint" => "mediumint",
        "int" | "integer" | "int4" | "serial" | "serial4" => "int",
        "int8" | "bigint" | "bigserial" | "serial8" => "bigint",
        "dec" | "decimal" | "numeric" => "numeric",
        "float4" | "real" => "real",
        "float8" | "double precision" | "double" | "float" => "float",
        "bool" | "boolean" => "boolean",
        "character varying" | "varchar" | "character" | "char" | "bpchar" | "text" | "citext"
            if engine.sql_syntax() == culprit_core::SqlSyntax::Postgres =>
        {
            "text"
        }
        "varchar" | "char" | "text" => "varchar",
        "nvarchar" | "nchar" | "ntext" | "national character varying" | "national character" => "nvarchar",
        "varbinary" | "binary" | "image" => "varbinary",
        "timestamp with time zone" | "timestamptz" => "timestamptz",
        "timestamp without time zone" | "timestamp" => "timestamp",
        other => other,
    };
    let mut out = if unsigned { format!("{family} unsigned") } else { family.to_string() };
    if let Some(cs) = charset {
        out.push_str(&format!(" charset {cs}"));
    }
    if let Some(coll) = collation {
        out.push_str(&format!(" collate {coll}"));
    }
    Some(out)
}

/// A foreign key column whose type differs from the parent column's it references.
pub struct TypeMismatch<'s> {
    fk: &'s ForeignKey,
    child: &'s culprit_core::ColumnInfo,
    parent: &'s culprit_core::ColumnInfo,
}

/// Foreign keys whose child and parent columns have incompatible data types (`int` vs `bigint`, `int unsigned` vs
/// `int`, `varchar` vs `nvarchar`).
fn fk_type_mismatches(snapshot: &Snapshot) -> Vec<TypeMismatch<'_>> {
    if crate::evidence::index_catalog(snapshot).is_some() || snapshot.columns.is_empty() {
        return Vec::new();
    }
    let engine = snapshot.server.engine;
    let is_partition =
        |fk: &ForeignKey| snapshot.table(&fk.schema, &fk.table).is_some_and(|t| t.partition_of.is_some());
    let find_col = |schema: &str, table: &str, col: &str| {
        snapshot.columns.iter().find(|c| {
            c.schema.eq_ignore_ascii_case(schema)
                && c.table.eq_ignore_ascii_case(table)
                && c.name.eq_ignore_ascii_case(col)
        })
    };
    let mut out: Vec<TypeMismatch> = Vec::new();
    for fk in enforced_foreign_keys(snapshot).filter(|fk| !is_partition(fk)) {
        for (c_name, p_name) in fk.columns.iter().zip(&fk.ref_columns) {
            let (Some(c_col), Some(p_col)) =
                (find_col(&fk.schema, &fk.table, c_name), find_col(&fk.ref_schema, &fk.ref_table, p_name))
            else {
                continue;
            };
            let (Some(c_canon), Some(p_canon)) =
                (canonical_fk_type(engine, &c_col.data_type), canonical_fk_type(engine, &p_col.data_type))
            else {
                continue;
            };
            if c_canon == p_canon {
                continue;
            }
            // One finding a child column, whichever keys it is in.
            if out.iter().any(|m| m.fk.schema == fk.schema && m.fk.table == fk.table && m.child.name == c_col.name) {
                continue;
            }
            out.push(TypeMismatch { fk, child: c_col, parent: p_col });
        }
    }
    out
}

fn type_mismatch_finding(m: TypeMismatch<'_>) -> Finding {
    let (fk, c_col, p_col) = (m.fk, m.child, m.parent);
    let child = format!("{}.{}", fk.schema, fk.table);
    let parent = format!("{}.{}", fk.ref_schema, fk.ref_table);
    let id_key = format!("type-mismatch:{child}({})", c_col.name);
    let mut f = finding("unindexed-fk", &id_key, Category::Indexes, subject(SubjectKind::Table, &child));
    f.title = t!(
        "check.unindexed_fk.title.type_mismatch",
        table = &fk.table,
        column = &c_col.name,
        column_type = &c_col.data_type,
        parent_table = &fk.ref_table,
        parent_column = &p_col.name,
        parent_type = &p_col.data_type,
        foreign_key = &fk.name
    );
    f.summary = [
        t!(
            "check.unindexed_fk.summary.type_mismatch",
            foreign_key = &fk.name,
            child = &child,
            column = &c_col.name,
            column_type = &c_col.data_type,
            parent = &parent,
            parent_column = &p_col.name,
            parent_type = &p_col.data_type
        ),
        t!("check.unindexed_fk.summary.type_mismatch_cost"),
    ]
    .join(" ");
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.label =
        t!("check.unindexed_fk.impact.type_mismatch", column_type = &c_col.data_type, parent_type = &p_col.data_type);
    f.confidence = Confidence::High;
    f.metrics.push(Metric::text(
        t!("check.unindexed_fk.metric.child_column"),
        format!("{}.{} ({})", fk.table, c_col.name, c_col.data_type),
    ));
    f.metrics.push(Metric::text(
        t!("check.unindexed_fk.metric.parent_column"),
        format!("{}.{} ({})", fk.ref_table, p_col.name, p_col.data_type),
    ));
    f.metrics.push(Metric::text(t!("check.unindexed_fk.metric.foreign_key"), fk.name.clone()));
    f.fix = Some(Fix {
        summary: t!(
            "check.unindexed_fk.fix.type_mismatch",
            child = &child,
            column = &c_col.name,
            parent = &parent,
            parent_column = &p_col.name
        ),
        sql: None,
        cost: Some(t!("check.unindexed_fk.fix_cost.type_mismatch")),
    });
    f.verify = vec![t!(
        "check.unindexed_fk.verify.type_mismatch",
        table = &fk.table,
        column = &c_col.name,
        parent_table = &fk.ref_table,
        parent_column = &p_col.name
    )];
    f
}

/// An indexed or foreign-key column of a large table that stores UUIDs as text.
pub struct UuidText<'s> {
    table: &'s culprit_core::TableStat,
    column: &'s culprit_core::ColumnInfo,
    rows: f64,
    /// The engine's own 16-byte type for them.
    native: NativeUuid,
}

/// Where each engine keeps a UUID in 16 bytes.
enum NativeUuid {
    Ysql,
    Postgres,
    SqlServer,
    Mysql,
}

impl NativeUuid {
    fn data_type(&self) -> &'static str {
        match self {
            NativeUuid::Ysql | NativeUuid::Postgres => "uuid",
            NativeUuid::SqlServer => "uniqueidentifier",
            NativeUuid::Mysql => "binary(16)",
        }
    }

    fn engine(&self) -> &'static str {
        match self {
            NativeUuid::Ysql => "yugabyte",
            NativeUuid::Postgres => "postgres",
            NativeUuid::SqlServer => "mssql",
            NativeUuid::Mysql => "mysql",
        }
    }
}

/// Indexed or foreign-key columns on large tables (`>= 100k` rows) that store UUIDs/GUIDs in character/text types
/// (`varchar(36)`, `char(36)`, `text`) instead of the engine's native 16-byte representation (`uuid`,
/// `uniqueidentifier`, `binary(16)`).
fn uuid_text_indexed_columns(snapshot: &Snapshot) -> Vec<UuidText<'_>> {
    let engine = snapshot.server.engine;
    if engine == Engine::Sqlite || crate::evidence::index_catalog(snapshot).is_some() || snapshot.columns.is_empty() {
        return Vec::new();
    }
    let native = || match engine {
        Engine::Yugabyte => Some(NativeUuid::Ysql),
        Engine::Postgres => Some(NativeUuid::Postgres),
        Engine::Mssql => Some(NativeUuid::SqlServer),
        Engine::Mysql => Some(NativeUuid::Mysql),
        Engine::Sqlite => None,
    };
    if native().is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for table in &snapshot.tables {
        if table.partition_of.is_some() {
            continue;
        }
        let Some(rows) = table.rows.filter(|r| *r >= 100_000.0) else {
            continue;
        };
        let is_indexed_or_fk = |col_name: &str| {
            snapshot
                .indexes_on(&table.schema, &table.name)
                .any(|idx| idx.key_names().iter().any(|k| k.eq_ignore_ascii_case(col_name)))
                || snapshot.foreign_keys.iter().any(|fk| {
                    fk.schema.eq_ignore_ascii_case(&table.schema)
                        && fk.table.eq_ignore_ascii_case(&table.name)
                        && fk.columns.iter().any(|c| c.eq_ignore_ascii_case(col_name))
                })
        };
        for column in snapshot.columns.iter().filter(|c| {
            c.schema.eq_ignore_ascii_case(&table.schema)
                && c.table.eq_ignore_ascii_case(&table.name)
                && is_uuid_text_column(c)
                && is_indexed_or_fk(&c.name)
        }) {
            out.extend(native().map(|native| UuidText { table, column, rows, native }));
        }
    }
    out
}

fn uuid_text_finding(u: UuidText<'_>) -> Finding {
    let (table, col, rows) = (u.table, u.column, u.rows);
    let native_type = u.native.data_type();
    let qual = table.qualified();
    let id_key = format!("uuid-text:{qual}({})", col.name);
    let mut f = finding("unindexed-fk", &id_key, Category::Indexes, subject(SubjectKind::Table, &qual));
    f.actionability = culprit_core::Actionability::OptionalImprovement;
    f.severity = Severity::Low;
    f.impact.score = base_score(Severity::Low);
    f.impact.label = t!("check.unindexed_fk.impact.uuid_text", column_type = &col.data_type, native = native_type);
    f.confidence = Confidence::High;
    f.title = t!(
        "check.unindexed_fk.title.uuid_text",
        table = &table.name,
        column = &col.name,
        column_type = &col.data_type,
        native = native_type,
        rows = format::count(rows)
    );
    f.summary = [
        t!(
            "check.unindexed_fk.summary.uuid_text",
            table = &qual,
            column = &col.name,
            rows = format::count(rows),
            column_type = &col.data_type
        ),
        t!("check.unindexed_fk.summary.uuid_text_cost", engine = u.native.engine()),
    ]
    .join(" ");
    f.metrics.push(Metric::new(t!("common.metric.rows_in_table"), rows, Unit::Rows));
    f.metrics.push(Metric::text(
        t!("check.unindexed_fk.metric.column"),
        format!("{}.{} ({})", table.name, col.name, col.data_type),
    ));
    f.metrics.push(Metric::text(t!("check.unindexed_fk.metric.recommended_type"), native_type.to_string()));
    f.fix = Some(Fix {
        summary: t!(
            "check.unindexed_fk.fix.uuid_text",
            table = &qual,
            column = &col.name,
            column_type = &col.data_type,
            native = native_type
        ),
        sql: None,
        cost: Some(t!("check.unindexed_fk.fix_cost.uuid_text")),
    });
    f.verify = vec![t!("check.unindexed_fk.verify.uuid_text", table = &qual, column = &col.name, native = native_type)];
    f
}

fn is_uuid_text_column(col: &culprit_core::ColumnInfo) -> bool {
    let dt = col.data_type.trim().to_ascii_lowercase();
    let name = col.name.trim().to_ascii_lowercase();
    let named_uuid = name == "uuid" || name.ends_with("_uuid") || name == "guid" || name.ends_with("_guid");
    let default_uuid = col.default.as_deref().is_some_and(|d| {
        let lower = d.to_ascii_lowercase();
        lower.contains("uuid") || lower.contains("newid")
    });
    let is_char_36 = matches!(
        dt.as_str(),
        "char(36)" | "varchar(36)" | "character(36)" | "character varying(36)" | "nchar(36)" | "nvarchar(36)"
    );
    let is_unbounded_or_wide_text = matches!(dt.as_str(), "text" | "varchar" | "character varying" | "nvarchar");
    (is_char_36 && (named_uuid || default_uuid)) || (is_unbounded_or_wide_text && named_uuid)
}

/// A foreign key no index serves, on a table large enough for a finding of its own, and what its finding concludes.
pub struct UnindexedKey<'s> {
    fk: &'s ForeignKey,
    rows: f64,
    /// The parent's deletes, each a full read of this table, where they come to an amount ([`FK_MEDIUM_PARENT_DELETES_A_DAY`]
    /// a day), and the days the counters cover.
    deletes: Option<(f64, f64)>,
    /// PostgreSQL: the parent's updates that weren't HOT, and the share that were.
    parent_cold: Option<(f64, f64)>,
    /// The statements the plans show reading this table by the key without an index, and their share of database time.
    slowed: Vec<&'s QueryStat>,
    impact: Option<QueryImpact>,
    /// The statements listed with it: those slowed, otherwise those that use the key.
    listed: Vec<&'s QueryStat>,
    /// The parent deleted nothing over a week of counters: the index would mostly serve joins.
    quiet_parent: Option<QuietParent<'s>>,
    /// The parent's rows, where so few that each key matches a large share of this table.
    lookup_parent_rows: Option<f64>,
    /// An index led by the key that its filter keeps from serving it.
    filtered_index: Option<&'s IndexStat>,
    /// SQLite, which checks a key only on connections that turn PRAGMA foreign_keys on.
    sqlite: bool,
    severity: Severity,
    score: f64,
    confidence: Confidence,
}

fn unindexed_key<'s>(snapshot: &'s Snapshot, fk: &'s ForeignKey, rows: f64) -> UnindexedKey<'s> {
    // Queries that go through the key, still running: a plain insert into the child only validates the key against the
    // parent's primary key, which needs no index here.
    let touching: Vec<&QueryStat> =
        through_foreign_key(snapshot, fk).into_iter().filter(|q| !crate::query_is_history(snapshot, q)).collect();
    let quiet_parent = parent_quiet(snapshot, fk);
    // SQLite checks foreign keys only on connections that turn PRAGMA foreign_keys on, which Culprit can't see.
    let sqlite = snapshot.server.engine == Engine::Sqlite;
    // Severity needs an amount, not the table's size alone: the parent's deletes, each a full read of this table, as a
    // rate over an hour or more of counters (or, below, the time of the statements the plans show slowed).
    let parent_deletes = snapshot.table(&fk.ref_schema, &fk.ref_table).and_then(|t| t.deletes).filter(|d| *d > 0.0);
    let deletes_a_day = parent_deletes.and_then(|d| counter_rate_per_day(snapshot, d));
    let measured = deletes_a_day.is_some_and(|d| d >= FK_MEDIUM_PARENT_DELETES_A_DAY);
    let cascading = matches!(fk.delete_rule.as_deref(), Some("CASCADE" | "SET NULL" | "SET DEFAULT"));
    let parent_cold = (snapshot.server.engine == Engine::Postgres)
        .then(|| {
            let pt = snapshot.table(&fk.ref_schema, &fk.ref_table)?;
            let updates = pt.updates.filter(|u| u.is_finite() && *u >= 10_000.0)?;
            let hot = pt.storage.as_ref()?.hot_updates.filter(|h| h.is_finite() && *h >= 0.0 && *h <= updates)?;
            let ratio = hot / updates;
            (ratio < 0.5).then_some((updates - hot, ratio))
        })
        .flatten();
    let parent_cold_measured =
        parent_cold.is_some_and(|(cold, _)| counter_rate_per_day(snapshot, cold).is_some_and(|d| d >= 10_000.0));
    let mut severity = if ((rows >= FK_MEDIUM_ROWS && (measured || parent_cold_measured))
        || (rows >= 100_000.0 && cascading && quiet_parent.is_none()))
        && !sqlite
    {
        Severity::Medium
    } else {
        Severity::Low
    };
    let mut score = base_score(severity) + (rows.log10() - 4.0).max(0.0) * 2.0;
    // The statements the missing index slows are this finding's (refine folds their heavy-query findings into it), so
    // their database time is its weight too, graded by the same amount rule. Only those whose plan shows it: naming the
    // key in the text isn't enough, since another index can serve the statement or the column can be another table's.
    let slowed: Vec<&QueryStat> =
        touching.iter().copied().filter(|q| plan_reads_whole(snapshot, q, &fk.table, &fk.columns)).collect();
    let impact = query_impact(snapshot, &slowed).filter(|i| i.share >= 0.01);
    if let Some(impact) = &impact {
        score = score_for_share(impact.share).max(score);
        severity = severity.max(severity_for_impact(snapshot, &slowed, impact));
    }
    // The statements it claims: those the plans show slowed when there are any (the share and the headline count these),
    // otherwise the ones that name the key with no captured plan to say otherwise, as evidence of the relation in use.
    let listed = if slowed.is_empty() { touching } else { slowed.clone() };
    UnindexedKey {
        fk,
        rows,
        deletes: parent_deletes.filter(|_| measured).zip(counter_days(snapshot)),
        parent_cold,
        slowed,
        impact,
        listed,
        confidence: if quiet_parent.is_some() { Confidence::Low } else { Confidence::High },
        quiet_parent,
        lookup_parent_rows: lookup_parent_rows(snapshot, fk),
        filtered_index: filtered_fk_index(snapshot, fk),
        sqlite,
        severity,
        score,
    }
}

fn unindexed_key_finding(snapshot: &Snapshot, k: UnindexedKey<'_>) -> Finding {
    let fk = k.fk;
    let child = format!("{}.{}", fk.schema, fk.table);
    let parent = format!("{}.{}", fk.ref_schema, fk.ref_table);
    let cols = format::list(&fk.columns);
    let mut f = finding(
        "unindexed-fk",
        &format!("{child}({})", fk.columns.join(",")),
        Category::Indexes,
        subject(SubjectKind::Table, &child),
    );
    f.facts.index_keys = fk.columns.clone();
    f.title = t!("check.unindexed_fk.title", table = &fk.table, columns = &cols, parent_table = &fk.ref_table);
    let mut summary = vec![
        t!("check.unindexed_fk.summary.check", parent = &parent, child = &child, rows = format::count(k.rows)),
        t!("check.unindexed_fk.summary.joins", table = &fk.table, parent_table = &fk.ref_table),
    ];
    if let Some(rule @ ("CASCADE" | "SET NULL" | "SET DEFAULT")) = fk.delete_rule.as_deref() {
        summary.push(t!(
            "check.unindexed_fk.summary.cascade",
            rule = rule,
            parent = &parent,
            child = &child,
            action = if rule == "CASCADE" { "delete" } else { "update" }
        ));
    }
    if let Some((cold, hot_ratio)) = k.parent_cold {
        summary.push(t!(
            "check.unindexed_fk.summary.parent_cold",
            parent = &parent,
            cold = format::count(cold),
            hot = format::percent(hot_ratio),
            child = &child
        ));
    }
    if let Some((deletes, days)) = k.deletes {
        summary.push(t!(
            "check.unindexed_fk.summary.deletes",
            parent = &parent,
            how_many = how_many(snapshot, deletes, days, culprit_core::key!("common.noun.row")),
            child = &child
        ));
    }
    if k.sqlite {
        f.caveats.push(t!("check.unindexed_fk.caveat.sqlite_key"));
    }
    f.impact.label = t!("check.unindexed_fk.impact", rows = format::count(k.rows));
    if let Some(impact) = &k.impact {
        apply_query_impact(&mut f, impact);
        summary.push(t!("check.unindexed_fk.summary.share", share = format::percent(impact.share)));
    }
    if let Some(quiet) = &k.quiet_parent {
        summary.push(quiet_parent_sentence(snapshot, quiet));
    }
    f.summary = summary.join(" ");
    f.severity = k.severity;
    f.impact.score = k.score;
    f.confidence = k.confidence;
    if let Some(parent_rows) = k.lookup_parent_rows {
        f.caveats.push(t!(
            "check.unindexed_fk.caveat.lookup_parent",
            parent = &parent,
            parent_rows = format::count(parent_rows),
            per_key = format::count(k.rows / parent_rows),
            child = &child
        ));
    }
    f.metrics.push(Metric::new(t!("common.metric.rows_in_table"), k.rows, Unit::Rows));
    f.metrics.push(Metric::text(t!("check.unindexed_fk.metric.foreign_key"), fk.name.clone()));
    if let Some(rule) = fk.delete_rule.as_deref() {
        f.metrics.push(Metric::text(t!("check.unindexed_fk.metric.on_delete"), format!("ON DELETE {rule}")));
    }
    f.queries = query_refs(&k.listed.iter().copied().take(5).collect::<Vec<_>>());
    f.fix = Some(Fix {
        summary: t!("check.unindexed_fk.fix", n = fk.columns.len(), child = &child),
        sql: Some(create_index_sql(snapshot, &fk.schema, &fk.table, &fk.columns, &[])),
        cost: write_cost(snapshot, &fk.schema, &fk.table),
    });
    f.verify = fk_verify(snapshot, fk, &k.slowed);
    if let Some(i) = k.filtered_index {
        f.caveats.push(filtered_caveat(i, &parent));
    }
    f
}

/// The statements that go through a foreign key, as its finding and the index advisor list them: those that use the
/// key (`advisor::uses_foreign_key`: a key column searched, a delete on the parent, a join of the two tables), except
/// one whose captured plan shows the index wouldn't serve it: a plan that reads the child some other way (the build
/// side of a hash join, a filter on another column) or never reads it at all. A plan that reads the child by the key
/// without an index ([`plan_reads_whole`]) is one it serves; a statement with no captured plan may be, and stays.
pub(crate) fn through_foreign_key<'a>(snapshot: &'a Snapshot, fk: &culprit_core::ForeignKey) -> Vec<&'a QueryStat> {
    queries_touching(snapshot, &fk.schema, &fk.table)
        .into_iter()
        .filter(|q| crate::advisor::uses_foreign_key(snapshot.server.engine, &q.text, fk))
        .filter(|q| q.plan.is_none() || plan_reads_whole(snapshot, q, &fk.table, &fk.columns))
        .collect()
}

/// How to check an unindexed foreign key's fix: where plans show statements reading the child by the key without an
/// index (the share the finding claims), explaining one of them again shows the new index in use, as the engine says
/// it; the next scan's catalog has the key indexed, so the finding is gone (a parent delete then finds the child's
/// rows through it, which no scan shows by itself); and those statements' time a run drops, between two scans on a
/// source of running totals ([`crate::queries::what_changed_between_scans`]).
pub(crate) fn fk_verify(snapshot: &Snapshot, fk: &ForeignKey, slowed: &[&QueryStat]) -> Vec<String> {
    let child = format!("{}.{}", fk.schema, fk.table);
    let parent = format!("{}.{}", fk.ref_schema, fk.ref_table);
    let engine = snapshot.server.engine;
    let name = index_name(&fk.table, &fk.columns);
    let name = if engine == Engine::Mssql { name } else { name.to_ascii_lowercase() };
    let n = slowed.len();
    let mut out = Vec::new();
    if let Some(first) = slowed.first() {
        out.push(t!(
            "check.unindexed_fk.verify.explain",
            n = n,
            query = &first.id,
            child = &child,
            index = &name,
            // SQLite's EXPLAIN QUERY PLAN says it in its own words (after the table's name or its alias).
            sqlite = if engine == Engine::Sqlite { "yes" } else { "no" }
        ));
    }
    out.push(t!("check.unindexed_fk.verify.rescan", parent = &parent, child = &child));
    // What the statements behind the share show: their time a run, which on running totals only two scans show.
    if !slowed.is_empty() {
        out.push(
            crate::queries::what_changed_between_scans(
                snapshot,
                &t!("check.unindexed_fk.verify.queries", n = n),
                &t!("check.unindexed_fk.verify.cheaper", n = n),
            )
            .unwrap_or_else(|| t!("check.unindexed_fk.verify.workload", n = n)),
        );
    }
    out
}

/// A parent table that has deleted nothing (and maybe updated nothing) over a week of counters.
pub(crate) struct QuietParent<'s> {
    schema: &'s str,
    table: &'s str,
    /// It updated nothing either.
    unchanged: bool,
}

/// Whether a key's parent table has deleted nothing, when PostgreSQL or MySQL counted its deletes for at least a week
/// ([`watched_long_enough`]): the check that needs the index runs only when a parent row is deleted or its key changes,
/// so then the index is for joins, not for the check. `None` when it deleted rows, or nobody knows.
fn parent_quiet<'s>(snapshot: &'s Snapshot, fk: &'s ForeignKey) -> Option<QuietParent<'s>> {
    let parent = snapshot.table(&fk.ref_schema, &fk.ref_table)?;
    if parent.deletes != Some(0.0) || !watched_long_enough(snapshot) {
        return None;
    }
    Some(QuietParent { schema: &fk.ref_schema, table: &fk.ref_table, unchanged: parent.updates == Some(0.0) })
}

fn quiet_parent_sentence(snapshot: &Snapshot, quiet: &QuietParent<'_>) -> String {
    t!(
        "check.unindexed_fk.summary.quiet_parent",
        parent = format!("{}.{}", quiet.schema, quiet.table),
        since = since_phrase(snapshot),
        unchanged = if quiet.unchanged { "yes" } else { "no" }
    )
}

/// The sentence that says a key's parent table has deleted nothing ([`parent_quiet`]), as the index advisor says it
/// too.
pub(crate) fn parent_never_deletes(snapshot: &Snapshot, fk: &ForeignKey) -> Option<String> {
    parent_quiet(snapshot, fk).map(|quiet| quiet_parent_sentence(snapshot, &quiet))
}

/// A parent table so small (a lookup table) that each of its keys matches a large share of the child's rows: an index
/// on the key makes a parent delete cheap, but a read of one key's rows would still scan. Its rows.
fn lookup_parent_rows(snapshot: &Snapshot, fk: &ForeignKey) -> Option<f64> {
    let parent_rows = snapshot.table(&fk.ref_schema, &fk.ref_table)?.rows.filter(|r| *r >= 1.0)?;
    (parent_rows <= 1.0 / culprit_core::INDEX_KEEPS_AT_MOST).then_some(parent_rows)
}

fn filtered_caveat(i: &IndexStat, parent: &str) -> String {
    t!(
        "check.unindexed_fk.caveat.filtered",
        index = &i.name,
        filter = i.filter.as_deref().unwrap_or_default(),
        parent = parent
    )
}

/// Many foreign keys unindexed, mostly on tables too small for a finding of their own.
pub struct UnindexedPattern<'s> {
    /// The keys the database checks, and those no index serves.
    total: usize,
    unindexed: usize,
    /// Those on tables under [`FK_OWN_FINDING_ROWS`] rows, with their rows, largest first.
    small: Vec<(&'s ForeignKey, f64)>,
    /// Those on larger tables (each a finding of its own), and on tables of unknown size.
    big: usize,
    unknown: usize,
    /// The filtered indexes that lead with a key but can't serve it.
    filtered: Vec<&'s IndexStat>,
}

/// One finding for the habit rather than the table: many foreign keys unindexed, mostly on tables too small for a
/// finding of their own. A schema helper or ORM that declares keys without indexes is the usual cause, and fixing it
/// there covers the keys still to come.
fn unindexed_fk_pattern<'s>(
    snapshot: &'s Snapshot,
    unindexed: &[(&'s ForeignKey, Option<f64>)],
) -> Option<UnindexedPattern<'s>> {
    let total = enforced_foreign_keys(snapshot).count();
    let small: Vec<(&ForeignKey, f64)> =
        unindexed.iter().filter_map(|(fk, rows)| rows.filter(|r| *r < FK_OWN_FINDING_ROWS).map(|r| (*fk, r))).collect();
    if small.len() < 3 || (unindexed.len() as f64) < total as f64 * 0.25 {
        return None;
    }
    Some(UnindexedPattern {
        total,
        unindexed: unindexed.len(),
        small,
        big: unindexed.iter().filter(|(_, rows)| rows.is_some_and(|r| r >= FK_OWN_FINDING_ROWS)).count(),
        unknown: unindexed.iter().filter(|(_, rows)| rows.is_none()).count(),
        filtered: unindexed.iter().filter_map(|(fk, _)| filtered_fk_index(snapshot, fk)).collect(),
    })
}

fn pattern_finding(snapshot: &Snapshot, p: UnindexedPattern<'_>) -> Finding {
    let under = format::count(FK_OWN_FINDING_ROWS);
    let (total, unindexed) = (p.total.to_string(), p.unindexed.to_string());
    let mut f =
        finding("unindexed-fk", "schema", Category::Indexes, subject(SubjectKind::Database, &snapshot.server.database));
    f.title = if p.unindexed == p.total {
        t!("check.unindexed_fk.title.pattern_all", total = &total)
    } else {
        t!("check.unindexed_fk.title.pattern", unindexed = &unindexed, total = &total)
    };
    let names = p
        .small
        .iter()
        .take(4)
        .map(|(fk, rows)| {
            t!(
                "check.unindexed_fk.pattern.key",
                key = format!("{}({})", fk.table, fk.columns.join(", ")),
                rows = format::count(*rows)
            )
        })
        .collect::<Vec<_>>();
    let elsewhere = [
        (p.big > 0).then(|| t!("check.unindexed_fk.pattern.big", n = p.big.to_string())),
        (p.unknown > 0).then(|| t!("check.unindexed_fk.pattern.unknown", n = p.unknown.to_string())),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let small = p.small.len().to_string();
    let listed = if p.small.len() <= 4 { "all" } else { "largest" };
    let mut summary = vec![
        if elsewhere.is_empty() {
            t!(
                "check.unindexed_fk.summary.pattern_all_small",
                count = &small,
                under = &under,
                listed = listed,
                names = format::list(&names)
            )
        } else {
            t!(
                "check.unindexed_fk.summary.pattern_small",
                count = &small,
                under = &under,
                elsewhere = format::list(&elsewhere),
                listed = listed,
                names = format::list(&names)
            )
        },
        t!("check.unindexed_fk.summary.pattern_check"),
    ];
    match p.filtered.as_slice() {
        [] => {}
        [one] => summary.push(t!("check.unindexed_fk.summary.pattern_filtered_one", index = &one.name)),
        many => summary.push(t!("check.unindexed_fk.summary.pattern_filtered", count = many.len().to_string())),
    }
    summary.push(t!("check.unindexed_fk.summary.pattern_habit"));
    f.summary = summary.join(" ");
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + (p.small.len() as f64).log2().min(4.0);
    f.impact.label = t!("check.unindexed_fk.impact.pattern", unindexed = &unindexed, total = &total);
    f.confidence = Confidence::High;
    if snapshot.server.engine == Engine::Sqlite {
        f.caveats.push(t!("check.unindexed_fk.caveat.sqlite_keys"));
    }
    f.metrics.push(Metric::new(t!("check.unindexed_fk.metric.unindexed"), p.unindexed as f64, Unit::Count));
    f.metrics.push(Metric::new(t!("check.unindexed_fk.metric.foreign_keys"), p.total as f64, Unit::Count));
    if !p.filtered.is_empty() {
        f.metrics.push(Metric::new(t!("check.unindexed_fk.metric.filtered"), p.filtered.len() as f64, Unit::Count));
    }
    let mut fix = vec![t!("check.unindexed_fk.fix.pattern")];
    if snapshot.server.engine == Engine::Postgres {
        fix.push(t!("check.unindexed_fk.fix.pattern_postgres"));
    }
    f.fix = Some(Fix {
        summary: fix.join(" "),
        sql: Some(
            p.small
                .iter()
                .map(|(fk, _)| create_index_sql(snapshot, &fk.schema, &fk.table, &fk.columns, &[]))
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        cost: Some(t!("check.unindexed_fk.fix_cost.pattern")),
    });
    f.verify = vec![t!("check.unindexed_fk.verify.pattern")];
    f.caveats.push(t!("check.unindexed_fk.caveat.pattern_never_deleted"));
    f
}
