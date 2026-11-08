//! Environment drift between two reports of the same engine: tables, columns, indexes, settings and version.

use std::collections::{BTreeMap, BTreeSet};

use culprit_core::i18n::Arg;
use culprit_core::{
    ColumnInfo, Drift, DriftItem, DriftKind, DriftPresence, DriftSide, Engine, IndexStat, Report, Snapshot, format,
    server_label, t,
};

use crate::support::supports_online_index;

/// Values that are measurements rather than configuration; they always differ.
pub const NOT_SETTINGS: &[&str] = &[
    "datfrozenxid_age",
    "pg_stat_statements.dealloc",
    "performance_schema_digest_lost",
    "query_store_options.current_storage_size_mb",
];

pub fn drift(a: &Report, b: &Report) -> Drift {
    let (sa, sb) = (&a.snapshot, &b.snapshot);
    let side = |s: &Snapshot| DriftSide {
        connection: s.connection.clone(),
        environment: s.environment,
        engine: s.server.engine,
        product: s.server.product.clone(),
        version: s.server.version.clone(),
        database: s.server.database.clone(),
        taken_at: s.taken_at.clone(),
    };
    let names = Names { a: &sa.connection, b: &sb.connection };
    if sa.server.engine != sb.server.engine {
        let title = t!(
            "view.drift.engines_differ",
            a = names.a,
            engine_a = sa.server.engine.to_string(),
            b = names.b,
            engine_b = sb.server.engine.to_string()
        );
        return Drift {
            a: side(sa),
            b: side(sb),
            summary: title.clone(),
            items: vec![DriftItem {
                kind: DriftKind::Version,
                presence: DriftPresence::Different,
                subject: "engine".into(),
                title,
                a: Some(sa.server.engine.label().into()),
                b: Some(sb.server.engine.label().into()),
                sql: None,
                sql_for: None,
            }],
        };
    }

    let mut items = Vec::new();
    let tables_a = table_set(sa);
    let tables_b = table_set(sb);
    tables(&names, sa, sb, &tables_a, &tables_b, &mut items);
    // The side that read no columns, or no indexes, where the other did: not compared.
    let without =
        |a_empty: bool, b_empty: bool| (a_empty != b_empty).then_some(if a_empty { names.a } else { names.b });
    let no_columns = without(sa.columns.is_empty(), sb.columns.is_empty());
    let no_indexes = without(sa.indexes.is_empty(), sb.indexes.is_empty());
    if no_columns.is_none() {
        columns(&names, sa, sb, &tables_a, &tables_b, &mut items);
    }
    if no_indexes.is_none() {
        indexes(&names, sa, sb, &tables_a, &tables_b, &mut items);
    }
    foreign_keys(&names, sa, sb, &tables_a, &tables_b, &mut items);
    constraints(&names, sa, sb, &tables_a, &tables_b, &mut items);
    triggers(&names, sa, sb, &tables_a, &tables_b, &mut items);
    settings(&names, sa, sb, &mut items);
    if sa.server.version != sb.server.version {
        let (va, vb) = (
            server_label(&sa.server.product, &sa.server.version),
            server_label(&sb.server.product, &sb.server.version),
        );
        items.push(DriftItem {
            kind: DriftKind::Version,
            presence: DriftPresence::Different,
            subject: "version".into(),
            title: t!("view.drift.version", a = names.a, version_a = &va, b = names.b, version_b = &vb),
            a: Some(va),
            b: Some(vb),
            sql: None,
            sql_for: None,
        });
    }
    items.sort_by(|x, y| {
        x.kind
            .cmp(&y.kind)
            .then((x.presence == DriftPresence::Renamed).cmp(&(y.presence == DriftPresence::Renamed)))
            .then(x.subject.to_ascii_lowercase().cmp(&y.subject.to_ascii_lowercase()))
    });
    let mut summary = vec![summarize(&items)];
    summary.extend(match (no_columns, no_indexes) {
        (Some(columns), Some(indexes)) => {
            Some(t!("view.drift.not_compared_both", columns = columns, indexes = indexes))
        }
        (Some(connection), None) => Some(t!("view.drift.not_compared_columns", connection = connection)),
        (None, Some(connection)) => Some(t!("view.drift.not_compared_indexes", connection = connection)),
        (None, None) => None,
    });
    Drift { a: side(sa), b: side(sb), summary: summary.join(" "), items }
}

struct Names<'a> {
    a: &'a str,
    b: &'a str,
}

fn key(schema: &str, name: &str) -> String {
    format!("{}.{}", schema.to_ascii_lowercase(), name.to_ascii_lowercase())
}

fn table_set(s: &Snapshot) -> BTreeMap<String, String> {
    s.tables.iter().map(|t| (key(&t.schema, &t.name), t.qualified())).collect()
}

fn tables(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    let describe = |s: &Snapshot, k: &str| {
        let t = s.tables.iter().find(|t| key(&t.schema, &t.name) == k)?;
        let indexes = s.indexes.iter().filter(|i| key(&i.schema, &i.table) == k).count();
        Some(match t.rows {
            Some(rows) => {
                t!("view.drift.table_shape", n = indexes, count = indexes.to_string(), rows = format::count(rows))
            }
            None => t!("view.drift.table_shape_unknown_rows", n = indexes, count = indexes.to_string()),
        })
    };
    for (k, name) in ta.iter().filter(|(k, _)| !tb.contains_key(*k)) {
        items.push(DriftItem {
            kind: DriftKind::Table,
            presence: DriftPresence::OnlyA,
            subject: name.clone(),
            title: t!("view.drift.only_in", subject = name, has = n.a, lacks = n.b),
            a: describe(sa, k),
            b: None,
            sql: None,
            sql_for: None,
        });
    }
    for (k, name) in tb.iter().filter(|(k, _)| !ta.contains_key(*k)) {
        items.push(DriftItem {
            kind: DriftKind::Table,
            presence: DriftPresence::OnlyB,
            subject: name.clone(),
            title: t!("view.drift.only_in", subject = name, has = n.b, lacks = n.a),
            a: None,
            b: describe(sb, k),
            sql: None,
            sql_for: None,
        });
    }
}

fn columns(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    let (ca, cb) = (columns_by_table(sa), columns_by_table(sb));
    let shape = |c: &ColumnInfo| format!("{}{}", c.data_type, if c.nullable { " NULL" } else { " NOT NULL" });
    for (table, display) in ta.iter().filter(|(k, _)| tb.contains_key(*k)) {
        let (Some(cols_a), Some(cols_b)) = (ca.get(table), cb.get(table)) else { continue };
        for (name, col) in cols_a {
            let subject = format!("{display}.{}", col.name);
            match cols_b.get(name) {
                None => items.push(DriftItem {
                    kind: DriftKind::Column,
                    presence: DriftPresence::OnlyA,
                    title: t!("view.drift.only_in", subject = &subject, has = n.a, lacks = n.b),
                    subject,
                    a: Some(shape(col)),
                    b: None,
                    sql: None,
                    sql_for: None,
                }),
                Some(other) if !same_type(&col.data_type, &other.data_type) => items.push(DriftItem {
                    kind: DriftKind::Column,
                    presence: DriftPresence::Different,
                    title: t!(
                        "view.drift.column_type",
                        column = &subject,
                        type_a = &col.data_type,
                        a = n.a,
                        type_b = &other.data_type,
                        b = n.b
                    ),
                    subject,
                    a: Some(shape(col)),
                    b: Some(shape(other)),
                    sql: None,
                    sql_for: None,
                }),
                Some(other) if col.nullable != other.nullable => items.push(DriftItem {
                    kind: DriftKind::Column,
                    presence: DriftPresence::Different,
                    title: t!(
                        "view.drift.column_nullable",
                        column = &subject,
                        has = if col.nullable { n.a } else { n.b },
                        lacks = if col.nullable { n.b } else { n.a }
                    ),
                    subject,
                    a: Some(shape(col)),
                    b: Some(shape(other)),
                    sql: None,
                    sql_for: None,
                }),
                Some(_) => {}
            }
        }
        for col in cols_b.iter().filter(|(name, _)| !cols_a.contains_key(*name)).map(|(_, c)| c) {
            let subject = format!("{display}.{}", col.name);
            items.push(DriftItem {
                kind: DriftKind::Column,
                presence: DriftPresence::OnlyB,
                title: t!("view.drift.only_in", subject = &subject, has = n.b, lacks = n.a),
                subject,
                a: None,
                b: Some(shape(col)),
                sql: None,
                sql_for: None,
            });
        }
    }
}

fn columns_by_table(s: &Snapshot) -> BTreeMap<String, BTreeMap<String, &ColumnInfo>> {
    let mut m: BTreeMap<String, BTreeMap<String, &ColumnInfo>> = BTreeMap::new();
    for c in &s.columns {
        m.entry(key(&c.schema, &c.table)).or_default().insert(c.name.to_ascii_lowercase(), c);
    }
    m
}

/// What makes two indexes the same for performance: table, key columns in order (with direction), included columns,
/// uniqueness and filter. Names don't matter.
fn signature(i: &IndexStat) -> String {
    let mut include: Vec<String> = i.include.iter().map(|c| c.to_ascii_lowercase()).collect();
    include.sort();
    let opclasses = i
        .opclasses
        .as_ref()
        .map(|ops| ops.iter().map(|o| o.to_ascii_lowercase()).collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
        key(&i.schema, &i.table),
        i.columns.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>().join(","),
        include.join(","),
        i.is_unique || i.is_primary_key,
        i.filter.as_deref().map(normalize_filter).unwrap_or_default(),
        i.is_disabled,
        opclasses,
        i.kind.to_ascii_lowercase(),
        i.docdb.as_ref().map(|d| d.hash_columns.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>()),
    )
}

fn normalize_filter(filter: &str) -> String {
    filter
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '[' | ']' | '"' | '`' | '(' | ')'))
        .collect::<String>()
        .to_ascii_lowercase()
}

fn definition(i: &IndexStat) -> String {
    let cols: Vec<String> = i
        .columns
        .iter()
        .enumerate()
        .map(|(idx, c)| match i.opclasses.as_ref().and_then(|ops| ops.get(idx)).filter(|o| !o.is_empty()) {
            Some(op) => format!("{c} {op}"),
            None => c.clone(),
        })
        .collect();
    let mut out = format!("({})", cols.join(", "));
    if let Some(native) = &i.docdb {
        out.push_str(&if native.hash_columns.is_empty() {
            " RANGE SHARDED".into()
        } else {
            format!(" HASH ({})", native.hash_columns.join(", "))
        });
    }
    if !i.include.is_empty() {
        out.push_str(&format!(" INCLUDE ({})", i.include.join(", ")));
    }
    if i.is_primary_key {
        out.push_str(" PRIMARY KEY");
    } else if i.is_unique {
        out.push_str(" UNIQUE");
    }
    if i.is_clustered && !i.is_primary_key {
        out.push_str(" CLUSTERED");
    }
    if let Some(filter) = &i.filter {
        out.push_str(&format!(" WHERE {filter}"));
    }
    if i.is_disabled {
        out.push_str(" DISABLED");
    }
    out
}

fn indexes(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    // Indexes of a table that exists on one side only are part of that table's difference.
    let shared = |i: &&IndexStat| {
        let k = key(&i.schema, &i.table);
        ta.contains_key(&k) && tb.contains_key(&k)
    };
    let ia: Vec<&IndexStat> = sa.indexes.iter().filter(shared).collect();
    let ib: Vec<&IndexStat> = sb.indexes.iter().filter(shared).collect();
    let mut matched_a: BTreeSet<usize> = BTreeSet::new();
    let mut matched_b: BTreeSet<usize> = BTreeSet::new();

    for (x, i) in ia.iter().enumerate() {
        let sig = signature(i);
        if let Some(y) = ib.iter().enumerate().position(|(y, j)| !matched_b.contains(&y) && signature(j) == sig) {
            matched_a.insert(x);
            matched_b.insert(y);
            let j = ib[y];
            if !i.name.eq_ignore_ascii_case(&j.name) {
                items.push(DriftItem {
                    kind: DriftKind::Index,
                    presence: DriftPresence::Renamed,
                    subject: format!("{}.{}", i.qualified_table(), i.name),
                    title: t!(
                        "view.drift.index_renamed",
                        table = i.qualified_table(),
                        index_a = &i.name,
                        a = n.a,
                        index_b = &j.name,
                        b = n.b
                    ),
                    a: Some(format!("{} {}", i.name, definition(i))),
                    b: Some(format!("{} {}", j.name, definition(j))),
                    sql: None,
                    sql_for: None,
                });
            }
        }
    }
    // Same name, different definition.
    let unmatched: Vec<usize> = (0..ia.len()).filter(|x| !matched_a.contains(x)).collect();
    for x in unmatched {
        let i = ia[x];
        let same_name = ib.iter().enumerate().position(|(y, j)| {
            !matched_b.contains(&y)
                && key(&i.schema, &i.table) == key(&j.schema, &j.table)
                && i.name.eq_ignore_ascii_case(&j.name)
        });
        if let Some(y) = same_name {
            matched_a.insert(x);
            matched_b.insert(y);
            let j = ib[y];
            items.push(DriftItem {
                kind: DriftKind::Index,
                presence: DriftPresence::Different,
                subject: format!("{}.{}", i.qualified_table(), i.name),
                title: t!(
                    "view.drift.index_different",
                    index = &i.name,
                    table = i.qualified_table(),
                    definition_a = definition(i),
                    a = n.a,
                    definition_b = definition(j),
                    b = n.b
                ),
                a: Some(definition(i)),
                b: Some(definition(j)),
                sql: None,
                sql_for: None,
            });
        }
    }
    for (_, i) in ia.iter().enumerate().filter(|(x, _)| !matched_a.contains(x)) {
        items.push(missing_index(i, n.a, n.b, sb, DriftPresence::OnlyA));
    }
    for (_, j) in ib.iter().enumerate().filter(|(y, _)| !matched_b.contains(y)) {
        items.push(missing_index(j, n.b, n.a, sa, DriftPresence::OnlyB));
    }
}

/// `index` exists in `has` and not in `lacks`, whose snapshot is `target`.
fn missing_index(index: &IndexStat, has: &str, lacks: &str, target: &Snapshot, presence: DriftPresence) -> DriftItem {
    let (table, keys) = (index.qualified_table(), index.key_names().join(", "));
    let title = if index.is_primary_key {
        t!("view.drift.missing_primary_key", table = table, keys = keys, has = has, lacks = lacks)
    } else if index.is_unique {
        t!("view.drift.missing_unique_index", table = table, keys = keys, has = has, lacks = lacks)
    } else {
        t!("view.drift.missing_index", table = table, keys = keys, has = has, lacks = lacks)
    };
    let definition = format!("{} {}", index.name, definition(index));
    let (a, b) = if presence == DriftPresence::OnlyA { (Some(definition), None) } else { (None, Some(definition)) };
    let sql = create_sql(target, index);
    DriftItem {
        kind: DriftKind::Index,
        presence,
        subject: format!("{}.{}", index.qualified_table(), index.name),
        title,
        a,
        b,
        sql_for: sql.as_ref().map(|_| lacks.to_string()),
        sql,
    }
}

/// The statement that creates `index` (as defined on the other side) in `target`. Primary keys and clustered indexes
/// reshape the table, so they are left to a person.
fn create_sql(target: &Snapshot, index: &IndexStat) -> Option<String> {
    if index.is_primary_key || index.is_clustered || index.columns.is_empty() {
        return None;
    }
    if target.server.engine == Engine::Yugabyte {
        // Native definitions preserve composite HASH groups, ordered keys, collations and operator classes.
        // Rebuilding one from column names alone changes its distribution and may create a write hotspot.
        return index
            .definition
            .as_ref()
            .filter(|_| index.docdb.is_some())
            .map(|sql| format!("{};", sql.trim_end_matches(';')));
    }
    let table = target.table(&index.schema, &index.table);
    let kind = table.and_then(|t| t.kind.as_deref());
    if index.is_unique && matches!(kind, Some("hypertable" | "timescale-unresolved")) {
        let hypertable = table?.storage.as_ref()?.hypertable.as_ref()?;
        let keys = index.key_names();
        if hypertable.dimensions.is_empty() || !hypertable.dimensions.iter().all(|d| keys.contains(&d.as_str())) {
            return None;
        }
    }
    let dialect = target.server.engine.spec().table_dialect(kind);
    let online = if supports_online_index(target) { culprit_core::Online::Yes } else { culprit_core::Online::No };
    Some(dialect.create_index(&culprit_core::NewIndex {
        target: &dialect.qualified(&[&index.schema, &index.table]),
        unique: index.is_unique,
        name: &index.name,
        keys: &index.columns,
        include: &index.include,
        filter: index.filter.as_deref(),
        online,
    }))
}

/// Column types compare without MySQL's integer display widths: 8.0.19+ dropped `int(11)` to `int`, MariaDB keeps it,
/// and the column stores the same values either way. `tinyint(1)` keeps its width: it reads as a boolean.
fn same_type(a: &str, b: &str) -> bool {
    fn plain(t: &str) -> String {
        let lower = t.trim().to_ascii_lowercase();
        for int in ["bigint", "mediumint", "smallint", "integer", "int"] {
            if let Some(rest) = lower.strip_prefix(int)
                && let Some(after) = rest.strip_prefix('(')
                && let Some(close) = after.find(')')
                && after[..close].chars().all(|c| c.is_ascii_digit())
            {
                return format!("{int}{}", &after[close + 1..]);
            }
        }
        lower
    }
    plain(a) == plain(b)
}

fn settings(n: &Names, sa: &Snapshot, sb: &Snapshot, items: &mut Vec<DriftItem>) {
    for s in sa.settings.iter().filter(|s| !NOT_SETTINGS.contains(&s.name.as_str())) {
        let Some(other) = sb.setting(&s.name) else { continue };
        if s.value.trim().eq_ignore_ascii_case(other.value.trim()) {
            continue;
        }
        items.push(DriftItem {
            kind: DriftKind::Setting,
            presence: DriftPresence::Different,
            subject: s.name.clone(),
            title: t!(
                "view.drift.setting",
                setting = &s.name,
                value_a = &s.value,
                a = n.a,
                value_b = &other.value,
                b = n.b
            ),
            a: Some(s.value.clone()),
            b: Some(other.value.clone()),
            sql: None,
            sql_for: None,
        });
    }
}

fn foreign_keys(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    let shared = |fk: &&culprit_core::ForeignKey| {
        let k = key(&fk.schema, &fk.table);
        ta.contains_key(&k) && tb.contains_key(&k)
    };
    let fk_sig = |fk: &culprit_core::ForeignKey| {
        format!(
            "{}|{}|{}|{}",
            key(&fk.schema, &fk.table),
            fk.columns.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>().join(","),
            key(&fk.ref_schema, &fk.ref_table),
            fk.ref_columns.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>().join(",")
        )
    };
    let fk_desc = |fk: &culprit_core::ForeignKey| {
        let base =
            format!("({}) -> {}.{}({})", fk.columns.join(", "), fk.ref_schema, fk.ref_table, fk.ref_columns.join(", "));
        match fk.delete_rule.as_deref() {
            Some(rule) => format!("{base} ON DELETE {rule}"),
            None => base,
        }
    };
    let fks_a: Vec<&culprit_core::ForeignKey> = sa.foreign_keys.iter().filter(shared).collect();
    let fks_b: Vec<&culprit_core::ForeignKey> = sb.foreign_keys.iter().filter(shared).collect();
    let mut matched_b = BTreeSet::new();

    for fk_a in &fks_a {
        let sig = fk_sig(fk_a);
        let table_display = format!("{}.{}", fk_a.schema, fk_a.table);
        if let Some(pos) =
            fks_b.iter().enumerate().position(|(idx, fk_b)| !matched_b.contains(&idx) && fk_sig(fk_b) == sig)
        {
            matched_b.insert(pos);
            let fk_b = fks_b[pos];
            if fk_a.delete_rule.is_some() || fk_b.delete_rule.is_some() {
                let ra = fk_a.delete_rule.as_deref().unwrap_or("NO ACTION");
                let rb = fk_b.delete_rule.as_deref().unwrap_or("NO ACTION");
                if !ra.eq_ignore_ascii_case(rb) {
                    items.push(DriftItem {
                        kind: DriftKind::Table,
                        presence: DriftPresence::Different,
                        subject: format!("{table_display}.{}", fk_a.name),
                        title: t!(
                            "view.drift.foreign_key_delete_rule",
                            key = &fk_a.name,
                            table = &table_display,
                            rule_a = ra,
                            a = n.a,
                            rule_b = rb,
                            b = n.b
                        ),
                        a: Some(fk_desc(fk_a)),
                        b: Some(fk_desc(fk_b)),
                        sql: None,
                        sql_for: None,
                    });
                }
            }
        } else {
            items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::OnlyA,
                subject: format!("{table_display}.{}", fk_a.name),
                title: t!(
                    "view.drift.foreign_key_only_in",
                    table = &table_display,
                    key = &fk_a.name,
                    definition = fk_desc(fk_a),
                    has = n.a,
                    lacks = n.b
                ),
                a: Some(fk_desc(fk_a)),
                b: None,
                sql: None,
                sql_for: None,
            });
        }
    }
    for (idx, fk_b) in fks_b.iter().enumerate() {
        if matched_b.contains(&idx) {
            continue;
        }
        let table_display = format!("{}.{}", fk_b.schema, fk_b.table);
        items.push(DriftItem {
            kind: DriftKind::Table,
            presence: DriftPresence::OnlyB,
            subject: format!("{table_display}.{}", fk_b.name),
            title: t!(
                "view.drift.foreign_key_only_in",
                table = &table_display,
                key = &fk_b.name,
                definition = fk_desc(fk_b),
                has = n.b,
                lacks = n.a
            ),
            a: None,
            b: Some(fk_desc(fk_b)),
            sql: None,
            sql_for: None,
        });
    }
}

fn constraints(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    let shared = |c: &&culprit_core::ConstraintIssue| {
        let k = key(&c.schema, &c.table);
        ta.contains_key(&k) && tb.contains_key(&k)
    };
    let state_label = |c: &culprit_core::ConstraintIssue| {
        if c.disabled {
            t!("view.drift.constraint_state.disabled")
        } else {
            t!("view.drift.constraint_state.untrusted")
        }
    };
    let ca: BTreeMap<String, &culprit_core::ConstraintIssue> = sa
        .constraints
        .iter()
        .filter(shared)
        .map(|c| (format!("{}|{}", key(&c.schema, &c.table), c.name.to_ascii_lowercase()), c))
        .collect();
    let cb: BTreeMap<String, &culprit_core::ConstraintIssue> = sb
        .constraints
        .iter()
        .filter(shared)
        .map(|c| (format!("{}|{}", key(&c.schema, &c.table), c.name.to_ascii_lowercase()), c))
        .collect();
    for (k, c) in &ca {
        if !cb.contains_key(k) {
            let subject = format!("{}.{}.{}", c.schema, c.table, c.name);
            let table = format!("{}.{}", c.schema, c.table);
            items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::Different,
                title: if c.disabled {
                    t!("view.drift.constraint_disabled_in_a", constraint = &c.name, table = table, a = n.a, b = n.b)
                } else {
                    t!("view.drift.constraint_untrusted_in_a", constraint = &c.name, table = table, a = n.a, b = n.b)
                },
                subject,
                a: Some(state_label(c)),
                b: Some(t!("view.drift.constraint_state.validated")),
                sql: None,
                sql_for: None,
            });
        }
    }
    for (k, c) in &cb {
        if !ca.contains_key(k) {
            let subject = format!("{}.{}.{}", c.schema, c.table, c.name);
            let table = format!("{}.{}", c.schema, c.table);
            items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::Different,
                title: if c.disabled {
                    t!("view.drift.constraint_disabled_in_b", constraint = &c.name, table = table, a = n.a, b = n.b)
                } else {
                    t!("view.drift.constraint_untrusted_in_b", constraint = &c.name, table = table, a = n.a, b = n.b)
                },
                subject,
                a: Some(t!("view.drift.constraint_state.validated")),
                b: Some(state_label(c)),
                sql: None,
                sql_for: None,
            });
        }
    }
}

fn triggers(
    n: &Names,
    sa: &Snapshot,
    sb: &Snapshot,
    ta: &BTreeMap<String, String>,
    tb: &BTreeMap<String, String>,
    items: &mut Vec<DriftItem>,
) {
    let map_triggers = |s: &Snapshot| -> BTreeMap<(String, String), (String, String)> {
        let mut m = BTreeMap::new();
        for t in &s.tables {
            let tk = key(&t.schema, &t.name);
            if !ta.contains_key(&tk) || !tb.contains_key(&tk) {
                continue;
            }
            if let Some(storage) = &t.storage {
                for (name, events) in storage.triggers() {
                    m.insert(
                        (tk.clone(), name.to_ascii_lowercase()),
                        (format!("{}.{}.{name}", t.schema, t.name), events.to_string()),
                    );
                }
            }
        }
        m
    };
    let tr_a = map_triggers(sa);
    let tr_b = map_triggers(sb);
    for (k, (subject, ev_a)) in &tr_a {
        match tr_b.get(k) {
            None => items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::OnlyA,
                subject: subject.clone(),
                title: t!("view.drift.trigger_only_in", trigger = subject, events = ev_a, has = n.a, lacks = n.b),
                a: Some(t!("view.drift.trigger_value", events = ev_a)),
                b: None,
                sql: None,
                sql_for: None,
            }),
            Some((_, ev_b)) if !ev_a.eq_ignore_ascii_case(ev_b) => items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::Different,
                subject: subject.clone(),
                title: t!(
                    "view.drift.trigger_events",
                    trigger = subject,
                    events_a = ev_a,
                    a = n.a,
                    events_b = ev_b,
                    b = n.b
                ),
                a: Some(t!("view.drift.trigger_value", events = ev_a)),
                b: Some(t!("view.drift.trigger_value", events = ev_b)),
                sql: None,
                sql_for: None,
            }),
            Some(_) => {}
        }
    }
    for (k, (subject, ev_b)) in &tr_b {
        if !tr_a.contains_key(k) {
            items.push(DriftItem {
                kind: DriftKind::Table,
                presence: DriftPresence::OnlyB,
                subject: subject.clone(),
                title: t!("view.drift.trigger_only_in", trigger = subject, events = ev_b, has = n.b, lacks = n.a),
                a: None,
                b: Some(t!("view.drift.trigger_value", events = ev_b)),
                sql: None,
                sql_for: None,
            });
        }
    }
}

fn summarize(items: &[DriftItem]) -> String {
    let real: Vec<&DriftItem> = items.iter().filter(|i| i.presence != DriftPresence::Renamed).collect();
    let renamed = items.len() - real.len();
    // Each kind's count, as a counted noun (`{n, plural, one {{count} index} other {{count} indexes}}`).
    let counted = |noun: &str, n: usize| {
        culprit_core::i18n::t(noun, &[("n", Arg::Number(n as f64)), ("count", Arg::Text(n.to_string()))])
    };
    let mut parts = Vec::new();
    for (kind, noun) in [
        (DriftKind::Index, culprit_core::key!("view.drift.count.index")),
        (DriftKind::Table, culprit_core::key!("view.drift.count.table")),
        (DriftKind::Column, culprit_core::key!("view.drift.count.column")),
        (DriftKind::Setting, culprit_core::key!("view.drift.count.setting")),
        (DriftKind::Version, culprit_core::key!("view.drift.count.version")),
    ] {
        let count = real.iter().filter(|i| i.kind == kind).count();
        if count > 0 {
            parts.push(counted(noun, count));
        }
    }
    let mut summary = vec![if real.is_empty() {
        t!("view.drift.none")
    } else {
        t!("view.drift.differences", n = real.len(), count = real.len().to_string(), parts = format::list(&parts))
    }];
    if renamed > 0 {
        summary.push(t!("view.drift.renamed", n = renamed, count = renamed.to_string()));
    }
    summary.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::Engine;

    #[test]
    fn unique_hypertable_index_requires_every_target_dimension() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/postgres-demo.json")).unwrap();
        let mut index = snapshot.indexes[0].clone();
        let table = snapshot.tables.iter_mut().find(|t| t.schema == index.schema && t.name == index.table).unwrap();
        table.kind = Some("hypertable".into());
        table.storage = Some(culprit_core::TableStorage {
            hypertable: Some(culprit_core::signals::HypertableStorage {
                dimensions: vec!["observed_at".into()],
                ..Default::default()
            }),
            ..Default::default()
        });
        index.is_primary_key = false;
        index.is_clustered = false;
        index.is_unique = true;
        index.columns = vec!["device_id".into()];
        assert!(create_sql(&snapshot, &index).is_none());
        index.columns.push("observed_at DESC".into());
        let sql = create_sql(&snapshot, &index).unwrap();
        assert!(
            sql.starts_with("CREATE UNIQUE INDEX")
                && !sql.contains("CONCURRENTLY")
                && !sql.contains("transaction_per_chunk")
        );
    }

    #[test]
    fn integer_display_widths_are_not_drift() {
        assert!(same_type("int", "int(11)"));
        assert!(same_type("bigint unsigned", "bigint(20) unsigned"));
        assert!(!same_type("tinyint(1)", "tinyint(4)"));
        assert!(!same_type("int", "bigint"));
        assert!(same_type("VARCHAR(20)", "varchar(20)"));
    }

    fn report(name: &str) -> Report {
        let text = include_str!("../../../../fixtures/mssql-demo.report.json");
        let mut r: Report = serde_json::from_str(text).unwrap();
        r.snapshot.connection = name.into();
        r
    }

    #[test]
    fn identical_reports_have_no_drift() {
        let d = drift(&report("alpha"), &report("prod"));
        assert!(d.items.is_empty(), "{:?}", d.items);
        assert_eq!(d.summary, "No differences in tables, columns, indexes or settings.");
    }

    #[test]
    fn finds_missing_renamed_and_changed_indexes_columns_and_settings() {
        let a = report("alpha");
        let mut b = report("prod");
        let s = &mut b.snapshot;
        let gone = s.tables.pop().unwrap();
        let kept = |schema: &str, table: &str| !(schema == gone.schema && table == gone.name);
        let dropped = s
            .indexes
            .iter()
            .position(|i| kept(&i.schema, &i.table) && !i.is_primary_key && !i.is_clustered && !i.is_unique)
            .unwrap();
        let dropped = s.indexes.remove(dropped);
        let renamed = s.indexes.iter_mut().find(|i| kept(&i.schema, &i.table) && !i.is_primary_key).unwrap();
        let old_name = renamed.name.clone();
        renamed.name = format!("{old_name}_v2");
        s.columns.iter_mut().find(|c| kept(&c.schema, &c.table)).unwrap().data_type = "bigint".into();
        let setting = &mut s.settings[0];
        setting.value = format!("{}-changed", setting.value);

        let d = drift(&a, &b);
        let missing =
            d.items.iter().find(|i| i.kind == DriftKind::Index && i.presence == DriftPresence::OnlyA).unwrap();
        assert!(missing.subject.ends_with(&dropped.name), "{missing:?}");
        assert_eq!(missing.sql_for.as_deref(), Some("prod"));
        assert!(missing.sql.as_deref().unwrap().contains(&dropped.name));
        assert!(missing.title.contains("in alpha that prod lacks"));
        assert!(d.items.iter().any(|i| i.presence == DriftPresence::Renamed && i.title.contains(&old_name)));
        assert!(d.items.iter().any(|i| i.kind == DriftKind::Column && i.title.contains("bigint")));
        assert!(d.items.iter().any(|i| i.kind == DriftKind::Setting));
        assert!(d.items.iter().any(|i| i.kind == DriftKind::Table && i.presence == DriftPresence::OnlyA));
        assert!(d.summary.starts_with(&format!("{} differences", d.items.len() - 1)), "{}", d.summary);
        assert!(d.summary.ends_with("One index only differs by name."), "{}", d.summary);
        assert_eq!(d.items[0].kind, DriftKind::Index, "indexes lead");
    }

    #[test]
    fn different_engines_are_not_compared() {
        let a = report("alpha");
        let mut b = report("prod");
        b.snapshot.server.engine = Engine::Postgres;
        let d = drift(&a, &b);
        assert_eq!(d.items.len(), 1);
        assert!(d.summary.contains("same engine"));
    }

    #[test]
    fn finds_foreign_key_constraint_and_trigger_drift() {
        let mut a = report("staging");
        let mut b = report("prod");
        a.snapshot.foreign_keys[0].delete_rule = Some("CASCADE".into());
        b.snapshot.foreign_keys[0].delete_rule = Some("NO_ACTION".into());
        let removed_fk = b.snapshot.foreign_keys.pop().unwrap();
        let tbl_schema = a.snapshot.tables[0].schema.clone();
        let tbl_name = a.snapshot.tables[0].name.clone();
        a.snapshot.tables[0].storage.get_or_insert_with(Default::default).options.push("trigger=trg_sync:IU".into());
        b.snapshot.constraints.push(culprit_core::ConstraintIssue {
            schema: tbl_schema,
            table: tbl_name,
            name: "ck_valid_amount".into(),
            kind: culprit_core::ConstraintKind::Check,
            disabled: false,
            inheritance_child: false,
        });

        let d = drift(&a, &b);
        assert!(
            d.items
                .iter()
                .any(|i| i.title.contains("uses ON DELETE CASCADE in staging and ON DELETE NO_ACTION in prod"))
        );
        assert!(d.items.iter().any(|i| i.title.contains(&removed_fk.name) && i.presence == DriftPresence::OnlyA));
        assert!(d.items.iter().any(|i| i.title.contains("trg_sync") && i.presence == DriftPresence::OnlyA));
        assert!(d.items.iter().any(|i| i.title.contains("ck_valid_amount") && i.presence == DriftPresence::Different));
    }
}
