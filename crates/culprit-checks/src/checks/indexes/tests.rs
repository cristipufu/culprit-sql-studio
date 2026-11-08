use super::*;

#[test]
fn hiding_filter_literals_preserves_equality_and_statement_coverage() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut template = s.indexes[0].clone();
    template.schema = "shop".into();
    template.table = "orders".into();
    template.columns = vec!["tenant".into()];
    template.is_primary_key = false;
    template.is_unique = false;
    template.is_clustered = false;
    template.is_disabled = false;
    s.indexes.clear();
    let filters = ["tenant = 'private-alpha'", "tenant = 'private-beta'", "tenant = 'private-alpha'", "is_deleted = 0"];
    for (i, filter) in filters.iter().enumerate() {
        let mut index = template.clone();
        index.name = format!("ix_{i}");
        index.filter = Some((*filter).into());
        s.indexes.push(index);
    }
    let w = s.workload.as_mut().unwrap();
    let query = w.queries[0].clone();
    w.queries = ["tenant = 'private-alpha'", "tenant = 'private-beta'", "is_deleted = 0", "is_deleted = @p"]
        .into_iter()
        .enumerate()
        .map(|(i, condition)| {
            let mut q = query.clone();
            q.id = i.to_string();
            q.text = format!("SELECT id FROM shop.orders WHERE {condition}");
            q
        })
        .collect();
    let shared = culprit_core::redact::redact_snapshot(&s);
    assert_eq!(
        shared.indexes[0].filter, shared.indexes[1].filter,
        "textual placeholders alone cannot prove equivalence"
    );
    assert!(!shared.indexes[0].same_filter(&shared.indexes[1]));
    assert!(shared.indexes[0].same_filter(&shared.indexes[2]));
    for (original, hidden) in s.indexes.iter().zip(&shared.indexes) {
        for (query, hidden_query) in s.queries().iter().zip(shared.queries()) {
            assert_eq!(fits_index_filter(original, &[query]), fits_index_filter(hidden, &[hidden_query]));
        }
    }
    assert_eq!(redundant_indexes(&s).len(), redundant_indexes(&shared).len());
    let output = serde_json::to_string(&shared).unwrap();
    assert!(!output.contains("private-alpha") && !output.contains("private-beta"));
}

#[test]
fn filters_made_of_literal_equalities() {
    let pairs =
        |f: &str| filter_equalities(f).map(|v| v.into_iter().map(|(c, l)| format!("{c}={l}")).collect::<Vec<_>>());
    assert_eq!(pairs("([is_deleted]=(0))"), Some(vec!["is_deleted=0".to_string()]));
    assert_eq!(
        pairs("([status]='open' AND [kind]=(2))"),
        Some(vec!["status='open'".to_string(), "kind=2".to_string()])
    );
    assert_eq!(pairs("([customer_id] IS NOT NULL)"), None);
    assert_eq!(pairs("([is_deleted]=(0) AND [due_at]>=getdate())"), None);
    assert_eq!(pairs("([status]<>'closed')"), None);
}

#[test]
fn a_parameter_compared_on_either_side() {
    assert!(compares_to_parameter(
        "select id from [t] where (([a] = @p0) and ([is_deleted] = @par1))",
        "t",
        "is_deleted"
    ));
    assert!(compares_to_parameter("SELECT id FROM t WHERE @deleted = t.is_deleted", "t", "is_deleted"));
    assert!(!compares_to_parameter("SELECT id FROM t WHERE is_deleted = 0 AND a = @p", "t", "is_deleted"));
    assert!(!compares_to_parameter("SELECT id FROM t WHERE is_deleted >= @p", "t", "is_deleted"));
    assert!(!compares_to_parameter("SELECT id FROM t WHERE was_is_deleted = @p", "t", "is_deleted"));
    assert!(!compares_to_parameter("SELECT id FROM t WHERE @p <= is_deleted", "t", "is_deleted"));
}

#[test]
fn a_literal_compared_on_either_side() {
    let fits = |text: &str, literal: &str| compared_to(text, "t", "status", |v| same_literal(v, literal));
    assert!(fits("SELECT id FROM t WHERE [t].[status] = N'open' AND a = @p", "'open'"));
    assert!(fits("SELECT id FROM t WHERE 'open' = status", "N'open'"));
    assert!(fits("SELECT id FROM t WHERE (status = (2))", "2"));
    assert!(fits("SELECT id FROM t WHERE status=2.0", "(2)"));
    assert!(!fits("SELECT id FROM t WHERE status = 'closed'", "'open'"));
    assert!(!fits("SELECT id FROM t WHERE status = @p", "'open'"));
    assert!(!fits("SELECT id FROM t WHERE status >= 2", "2"));
    assert!(!fits("SELECT id FROM t WHERE 2 <= status", "2"));
    assert!(!fits("SELECT id FROM t WHERE a = 2", "2"));
}

// A staging scan's dispatch_intents: a join compares the other table's is_deleted with a value, not this one's. A column
// qualified by this table, or by the alias the statement gives it, is this table's.
#[test]
fn a_column_qualified_by_another_table_is_that_tables() {
    let deleted = |text: &str| compared_to(text, "dispatch_intents", "is_deleted", |v| same_literal(v, "0"));
    assert!(!deleted(
        "select [i].[id] from [app].[call_sessions] left join [app].[dispatch_intents] on [app].[dispatch_intents].[call_session_id] = [app].[call_sessions].[id] where [app].[call_sessions].[is_deleted] = 0"
    ));
    assert!(deleted("select id from [app].[dispatch_intents] where [app].[dispatch_intents].[is_deleted] = 0"));
    assert!(deleted(
        "select i.id from app.call_sessions s join app.dispatch_intents i on i.call_session_id = s.id where i.is_deleted = 0"
    ));
    assert!(!deleted(
        "select i.id from app.call_sessions s join app.dispatch_intents i on i.call_session_id = s.id where s.is_deleted = 0"
    ));
    assert!(deleted("select id from dispatch_intents where is_deleted = 0"));
}

#[test]
fn orphaned_invalid_postgres_index_is_reported_and_redundant_invalid_index_is_duplicate() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(10.0 * 24.0);
    for i in &mut s.indexes {
        i.counters_since = None;
    }
    let mut invalid = s.indexes.iter().find(|i| i.table == "orders" && !i.is_primary_key).unwrap().clone();
    invalid.name = "orders_abandoned_ccnew".into();
    invalid.columns = vec!["shipping_cents".into()];
    invalid.is_disabled = true;
    invalid.maintained = Some(true);
    invalid.size_bytes = Some(8.0 * 1024.0 * 1024.0);
    s.indexes.push(invalid);
    let out = unused_index(&s);
    let f = out
        .iter()
        .find(|f| f.id == "unused-index:invalid:public.orders.orders_abandoned_ccnew")
        .expect("orphaned invalid index");
    assert!(f.summary.contains("indisvalid = false"), "{}", f.summary);
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("DROP INDEX CONCURRENTLY"));

    // When a valid index already covers the same keys, duplicate-index flags the leftover invalid index immediately.
    let valid_col =
        s.indexes.iter().find(|i| i.table == "orders" && !i.is_primary_key && !i.is_disabled).unwrap().columns.clone();
    s.indexes.last_mut().unwrap().columns = valid_col;
    let dup = duplicate_index(&s);
    assert!(
        dup.iter().any(|f| f.id == "duplicate-index:public.orders.orders_abandoned_ccnew"),
        "{:?}",
        dup.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
}

#[test]
fn foreign_key_column_type_mismatch_is_flagged_without_false_positives_on_aliases_or_lengths() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    // Standard demo fixture has matching types (and aliases like int4 vs integer or varchar(50) vs text must not fire).
    assert!(!unindexed_fk(&s).iter().any(|f| f.id.contains("type-mismatch")));

    let fk = s.foreign_keys[0].clone();
    let child_col =
        s.columns.iter_mut().find(|c| c.schema == fk.schema && c.table == fk.table && c.name == fk.columns[0]).unwrap();
    child_col.data_type = "integer".into();
    let parent_col = s
        .columns
        .iter_mut()
        .find(|c| c.schema == fk.ref_schema && c.table == fk.ref_table && c.name == fk.ref_columns[0])
        .unwrap();
    parent_col.data_type = "int4".into();
    assert!(!unindexed_fk(&s).iter().any(|f| f.id.contains("type-mismatch")), "int4 and integer are the same type");

    let parent_col = s
        .columns
        .iter_mut()
        .find(|c| c.schema == fk.ref_schema && c.table == fk.ref_table && c.name == fk.ref_columns[0])
        .unwrap();
    parent_col.data_type = "bigint".into();
    let findings = unindexed_fk(&s);
    let mismatch = findings.iter().find(|f| f.id.starts_with("unindexed-fk:type-mismatch:")).expect("int vs bigint");
    assert!(mismatch.title.contains("(integer)") && mismatch.title.contains("(bigint)"), "{}", mismatch.title);

    // ON DELETE CASCADE on an unindexed FK elevates severity and calls out cascade amplification.
    s.foreign_keys[0].delete_rule = Some("CASCADE".into());
    let findings = unindexed_fk(&s);
    let cascade_fk = findings
        .iter()
        .find(|f| f.id.starts_with("unindexed-fk:") && !f.id.contains("type-mismatch:") && !f.id.contains("uuid-text:"))
        .expect("unindexed fk finding");
    assert!(cascade_fk.summary.contains("ON DELETE CASCADE"), "{}", cascade_fk.summary);

    // UUID stored as varchar(36) on an indexed column of a large table is flagged.
    let tbl_schema = s.tables[0].schema.clone();
    let tbl_name = s.tables[0].name.clone();
    s.tables[0].rows = Some(250_000.0);
    s.columns.push(culprit_core::ColumnInfo {
        schema: tbl_schema.clone(),
        table: tbl_name.clone(),
        name: "external_uuid".into(),
        data_type: "varchar(36)".into(),
        nullable: false,
        default: None,
        generated: false,
        ordinal: 88,
    });
    let mut ix = s.indexes[0].clone();
    ix.schema = tbl_schema.clone();
    ix.table = tbl_name.clone();
    ix.name = "idx_external_uuid".into();
    ix.columns = vec!["external_uuid".into()];
    ix.is_disabled = false;
    s.indexes.push(ix);
    let findings = unindexed_fk(&s);
    let uuid_f = findings
        .iter()
        .find(|f| f.id == format!("unindexed-fk:uuid-text:{tbl_schema}.{tbl_name}(external_uuid)"))
        .expect("uuid text finding");
    assert!(uuid_f.title.contains("stores UUIDs as varchar(36)"), "{}", uuid_f.title);

    // Collation/charset mismatch on string foreign keys is also caught by fk_type_mismatches.
    let child_col =
        s.columns.iter_mut().find(|c| c.schema == fk.schema && c.table == fk.table && c.name == fk.columns[0]).unwrap();
    child_col.data_type = "varchar(36) COLLATE utf8mb4_unicode_ci".into();
    let parent_col = s
        .columns
        .iter_mut()
        .find(|c| c.schema == fk.ref_schema && c.table == fk.ref_table && c.name == fk.ref_columns[0])
        .unwrap();
    parent_col.data_type = "varchar(36)".into();
    let findings = unindexed_fk(&s);
    let coll_mismatch =
        findings.iter().find(|f| f.id.starts_with("unindexed-fk:type-mismatch:")).expect("collation mismatch");
    assert!(coll_mismatch.title.contains("COLLATE utf8mb4_unicode_ci"), "{}", coll_mismatch.title);

    // A broad Index Seek with a selective residual filter on an unindexed second column produces a filtered-scan finding.
    let mut seek_snap: Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    let target_schema = seek_snap.tables[0].schema.clone();
    let target_table = seek_snap.tables[0].name.clone();
    let mut tenant_ix = seek_snap.indexes[0].clone();
    tenant_ix.schema = target_schema.clone();
    tenant_ix.table = target_table.clone();
    tenant_ix.name = "ix_tenant".into();
    tenant_ix.columns = vec!["tenant_id".into()];
    tenant_ix.is_disabled = false;
    seek_snap.indexes.retain(|i| !(i.schema == target_schema && i.table == target_table));
    seek_snap.indexes.push(tenant_ix);
    if let Some(w) = seek_snap.workload.as_mut()
        && let Some(q) = w.queries.iter_mut().find(|q| q.plan.is_some())
    {
        q.total_time_ms = w.total_time_ms * 0.5;
        q.mean_time_ms = 250.0;
        q.executions = 500.0;
        if let Some(plan) = q.plan.as_mut() {
            plan.root.kind = NodeKind::Seek;
            plan.root.op = "Index Scan".into();
            plan.root.object = Some(format!("{target_schema}.{target_table}"));
            plan.root.index = Some("ix_tenant".into());
            plan.root.seek_predicate = Some("(tenant_id = $1)".into());
            plan.root.predicate = Some("(status = 'open')".into());
            plan.root.rows_read = Some(50_000.0);
            plan.root.est_rows = 50.0;
            plan.root.act_rows = Some(50.0);
            plan.root.est_executions = 1.0;
            plan.root.act_executions = Some(1.0);
            plan.root.children.clear();
        }
    }
    let scan_findings = filtered_scan(&seek_snap);
    assert!(
        scan_findings.iter().any(|f| f.id == format!("filtered-scan:{target_schema}.{target_table}(tenant_id,status)")),
        "{scan_findings:#?}"
    );
}
#[test]
fn yugabyte_absence_needs_complete_cluster_counters_and_their_own_reset_dates() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/yugabyte-demo.json")).unwrap();
    let index = s.indexes.iter().find(|i| !i.is_primary_key).unwrap().clone();
    s.indexes = vec![index];
    s.indexes[0].reads = Some(0.0);
    s.indexes[0].size_bytes = Some(1024.0 * 1024.0);
    s.server.started_at = Some("2026-01-01T00:00:00Z".into());
    assert!(unused_index(&s).is_empty(), "an old coordinator does not date index usage");
    let usage = s.sources.iter_mut().find(|o| o.source == culprit_core::source_id::DOCDB_INDEX_USAGE).unwrap();
    usage.state = culprit_core::SourceState::Collected;
    usage.reason = None;
    assert!(unused_index(&s).is_empty(), "complete totals with unknown reset dates cannot establish a week");
    s.indexes[0].counters_since = Some("2026-10-15T00:00:00Z".into());
    assert_eq!(unused_index(&s).len(), 1, "every node has now watched at least a week");
    assert_eq!(crate::support::counter_days(&s), None, "independent epochs do not supply a common rate denominator");
    s.sources.iter_mut().find(|o| o.source == culprit_core::source_id::DOCDB_INDEX_USAGE).unwrap().state =
        culprit_core::SourceState::Partial;
    assert!(unused_index(&s).is_empty(), "a skipped node can contain the missing reads");
}

/// What widening, bringing back and an index's write cost say, word for word, on every engine and in every case: their
/// words come from the language catalog, and this holds the English to what the code said when it wrote them itself.
#[test]
fn widening_bringing_back_and_write_costs_say_what_they_said() {
    let base: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut plain = base.indexes.iter().find(|i| !i.is_primary_key && !i.is_clustered).unwrap().clone();
    (plain.schema, plain.table, plain.name) = ("shop".into(), "orders".into(), "ix_orders_status".into());
    (plain.columns, plain.include, plain.filter) = (vec!["status".into()], vec!["total".into()], None);
    (plain.is_unique, plain.is_disabled, plain.size_bytes) = (false, false, Some(4.5 * 1024.0 * 1024.0));
    let cols = |c: &[&str]| c.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    let words = |w: Widening| match w {
        Widening::Sql(fix) => format!("sql: {} | cost: {}", fix.summary, fix.cost.unwrap_or_default()),
        Widening::Refused(why) => format!("refused: {why}"),
    };
    let mut out = Vec::new();
    for engine in Engine::ALL {
        for version in ["16.0", "11.4", "MariaDB"] {
            let mut s = base.clone();
            s.server.engine = engine;
            s.server.version = version.into();
            if version == "MariaDB" {
                s.server.product = "MariaDB".into();
                s.server.version = "10.11.6-MariaDB".into();
            }
            let tag = format!("{engine:?} {}", s.server.version);
            out.push(format!("{tag} held: {}", words(widen_index(&s, &plain, &cols(&["total"])))));
            out.push(format!("{tag} add: {}", words(widen_index(&s, &plain, &cols(&["created_at", "total"])))));
            let mut unique = plain.clone();
            unique.is_unique = true;
            out.push(format!("{tag} unique: {}", words(widen_index(&s, &unique, &cols(&["created_at"])))));
            let mut clustered = plain.clone();
            clustered.is_clustered = true;
            out.push(format!("{tag} clustered: {}", words(widen_index(&s, &clustered, &cols(&["created_at"])))));
            let mut pk = plain.clone();
            pk.is_primary_key = true;
            out.push(format!("{tag} pk: {}", words(widen_index(&s, &pk, &cols(&["created_at"])))));
            let mut off = plain.clone();
            off.is_disabled = true;
            out.push(format!("{tag} off: {}", words(widen_index(&s, &off, &cols(&["created_at"])))));
            out.push(format!("{tag} off-held: {}", words(widen_index(&s, &off, &cols(&["total"])))));
            let mut off_pk = off.clone();
            off_pk.is_primary_key = true;
            out.push(format!("{tag} off-pk one: {}", words(widen_index(&s, &off_pk, &cols(&["created_at"])))));
            out.push(format!(
                "{tag} off-pk two: {}",
                words(widen_index(&s, &off_pk, &cols(&["created_at", "channel"])))
            ));
            let fix = widen_fix(&s, &clustered, &cols(&["created_at"]));
            out.push(format!("{tag} widen_fix: {} | {:?}", fix.summary, fix.cost));
        }
    }
    // What an index costs the table's writes: none in a week of counters, none in counters that may have been reset,
    // none in too short a time, and some.
    let mut s = base.clone();
    let table = s.tables[0].clone();
    let (schema, name) = (table.schema.clone(), table.name.clone());
    s.indexes.retain(|i| !(i.schema == schema && i.table == name));
    let set = |s: &mut Snapshot, writes: f64| {
        let t = s.tables.iter_mut().find(|t| t.schema == schema && t.name == name).unwrap();
        (t.inserts, t.updates, t.deletes) = (Some(writes), Some(0.0), Some(0.0));
    };
    s.server.started_at = Some("2026-10-01T12:00:00Z".into());
    s.taken_at = "2026-10-11T12:00:00Z".into();
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    set(&mut s, 0.0);
    out.push(format!("week none: {:?}", write_cost(&s, &schema, &name)));
    out.push(format!("week none, any: {:?}", write_cost_of(&s, &schema, &name, "any index you add")));
    set(&mut s, 50_000.0);
    out.push(format!("week some: {:?}", write_cost(&s, &schema, &name)));
    set(&mut s, 1.0);
    out.push(format!("week one: {:?}", write_cost(&s, &schema, &name)));
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(10.0);
    s.server.started_at = Some("2026-10-11T02:00:00Z".into());
    set(&mut s, 0.0);
    out.push(format!("short none: {:?}", write_cost(&s, &schema, &name)));
    set(&mut s, 700.0);
    out.push(format!("short some: {:?}", write_cost(&s, &schema, &name)));
    // performance_schema's counters, reset since the server started: no length is said.
    let mut m: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mysql-demo.json")).unwrap();
    m.server.started_at = Some("2026-10-11T10:00:00Z".into());
    let t = m.tables.iter_mut().find(|t| t.name == "orders").unwrap();
    (t.inserts, t.updates, t.deletes) = (Some(0.0), Some(0.0), Some(0.0));
    m.indexes.retain(|i| i.table != "orders");
    out.push(format!("reset none: {:?}", write_cost(&m, "shop", "orders")));
    let actual = out.join("\n");
    assert!(actual == WIDENING_WORDS, "said:\n{actual}");
}

const WIDENING_WORDS: &str = r#"Mssql 16.0 held: refused: ix_orders_status already has these keys and includes total.
Mssql 16.0 add: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 16.0 unique: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. It stays unique. If ix_orders_status enforces a UNIQUE constraint rather than being a unique index, SQL Server refuses the rebuild (error 1907): create a separate index with these keys and included columns instead. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 16.0 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mssql 16.0 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mssql 16.0 off: sql: ix_orders_status has the right keys but is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back, with created_at included so it covers the query; if it was disabled because nothing read it, the queries below now would. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 16.0 off-held: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. | cost: 
Mssql 16.0 off-pk one: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mssql 16.0 off-pk two: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mssql 16.0 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Mssql 11.4 held: refused: ix_orders_status already has these keys and includes total.
Mssql 11.4 add: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 11.4 unique: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. It stays unique. If ix_orders_status enforces a UNIQUE constraint rather than being a unique index, SQL Server refuses the rebuild (error 1907): create a separate index with these keys and included columns instead. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 11.4 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mssql 11.4 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mssql 11.4 off: sql: ix_orders_status has the right keys but is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back, with created_at included so it covers the query; if it was disabled because nothing read it, the queries below now would. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 11.4 off-held: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. | cost: 
Mssql 11.4 off-pk one: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mssql 11.4 off-pk two: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mssql 11.4 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Mssql 10.11.6-MariaDB held: refused: ix_orders_status already has these keys and includes total.
Mssql 10.11.6-MariaDB add: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 10.11.6-MariaDB unique: sql: ix_orders_status already has the right keys but makes every matching row go back to the table for created_at. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. It stays unique. If ix_orders_status enforces a UNIQUE constraint rather than being a unique index, SQL Server refuses the rebuild (error 1907): create a separate index with these keys and included columns instead. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 10.11.6-MariaDB clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mssql 10.11.6-MariaDB pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mssql 10.11.6-MariaDB off: sql: ix_orders_status has the right keys but is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back, with created_at included so it covers the query; if it was disabled because nothing read it, the queries below now would. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. | cost: The index grows from 4.5 MB; writes that touch the included columns maintain it.
Mssql 10.11.6-MariaDB off-held: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. | cost: 
Mssql 10.11.6-MariaDB off-pk one: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mssql 10.11.6-MariaDB off-pk two: sql: ix_orders_status has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mssql 10.11.6-MariaDB widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Postgres 16.0 held: refused: ix_orders_status already has these keys and includes total.
Postgres 16.0 add: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 16.0 unique: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 16.0 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Postgres 16.0 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Postgres 16.0 off: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 16.0 off-held: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would. | cost: 
Postgres 16.0 off-pk one: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 16.0 off-pk two: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Postgres 16.0 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Postgres 11.4 held: refused: ix_orders_status already has these keys and includes total.
Postgres 11.4 add: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 11.4 unique: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 11.4 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Postgres 11.4 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Postgres 11.4 off: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 11.4 off-held: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. | cost: 
Postgres 11.4 off-pk one: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 11.4 off-pk two: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Postgres 11.4 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Postgres 10.11.6-MariaDB held: refused: ix_orders_status already has these keys and includes total.
Postgres 10.11.6-MariaDB add: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 10.11.6-MariaDB unique: refused: PostgreSQL can't add included columns to ix_orders_status in place; a new index has to be built.
Postgres 10.11.6-MariaDB clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Postgres 10.11.6-MariaDB pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Postgres 10.11.6-MariaDB off: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 10.11.6-MariaDB off-held: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. | cost: 
Postgres 10.11.6-MariaDB off-pk one: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Postgres 10.11.6-MariaDB off-pk two: sql: ix_orders_status has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable (this version can't rebuild concurrently: writes to the table wait meanwhile); if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Postgres 10.11.6-MariaDB widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Yugabyte 16.0 held: refused: ix_orders_status already has these keys and includes total.
Yugabyte 16.0 add: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 16.0 unique: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 16.0 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Yugabyte 16.0 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Yugabyte 16.0 off: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 16.0 off-held: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. | cost: 
Yugabyte 16.0 off-pk one: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 16.0 off-pk two: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Yugabyte 16.0 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Yugabyte 11.4 held: refused: ix_orders_status already has these keys and includes total.
Yugabyte 11.4 add: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 11.4 unique: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 11.4 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Yugabyte 11.4 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Yugabyte 11.4 off: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 11.4 off-held: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. | cost: 
Yugabyte 11.4 off-pk one: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 11.4 off-pk two: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Yugabyte 11.4 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Yugabyte 10.11.6-MariaDB held: refused: ix_orders_status already has these keys and includes total.
Yugabyte 10.11.6-MariaDB add: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 10.11.6-MariaDB unique: refused: YSQL cannot add included columns to ix_orders_status in place; build a separate covering index and verify its distributed cost.
Yugabyte 10.11.6-MariaDB clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Yugabyte 10.11.6-MariaDB pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Yugabyte 10.11.6-MariaDB off: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 10.11.6-MariaDB off-held: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. | cost: 
Yugabyte 10.11.6-MariaDB off-pk one: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Yugabyte 10.11.6-MariaDB off-pk two: sql: ix_orders_status has these keys, but the DocDB index is not valid yet; check backfill progress on every node before repairing a failed build, and let an active backfill finish; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Yugabyte 10.11.6-MariaDB widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Mysql 16.0 held: refused: ix_orders_status already has these keys and includes total.
Mysql 16.0 add: refused: MySQL has no included columns: an index there holds only its keys.
Mysql 16.0 unique: refused: MySQL has no included columns: an index there holds only its keys.
Mysql 16.0 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mysql 16.0 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mysql 16.0 off: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 16.0 off-held: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. | cost: 
Mysql 16.0 off-pk one: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 16.0 off-pk two: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mysql 16.0 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Mysql 11.4 held: refused: ix_orders_status already has these keys and includes total.
Mysql 11.4 add: refused: MySQL has no included columns: an index there holds only its keys.
Mysql 11.4 unique: refused: MySQL has no included columns: an index there holds only its keys.
Mysql 11.4 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mysql 11.4 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mysql 11.4 off: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 11.4 off-held: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. | cost: 
Mysql 11.4 off-pk one: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 11.4 off-pk two: sql: ix_orders_status has these keys, but it is invisible to the optimizer. Making it visible again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mysql 11.4 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Mysql 10.11.6-MariaDB held: refused: ix_orders_status already has these keys and includes total.
Mysql 10.11.6-MariaDB add: refused: MariaDB has no included columns: an index there holds only its keys.
Mysql 10.11.6-MariaDB unique: refused: MariaDB has no included columns: an index there holds only its keys.
Mysql 10.11.6-MariaDB clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Mysql 10.11.6-MariaDB pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Mysql 10.11.6-MariaDB off: sql: ix_orders_status has these keys, but the optimizer is told to ignore it. Letting it use the index again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 10.11.6-MariaDB off-held: sql: ix_orders_status has these keys, but the optimizer is told to ignore it. Letting it use the index again is instant; if it was set aside because nothing read it, the queries below now would. | cost: 
Mysql 10.11.6-MariaDB off-pk one: sql: ix_orders_status has these keys, but the optimizer is told to ignore it. Letting it use the index again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Mysql 10.11.6-MariaDB off-pk two: sql: ix_orders_status has these keys, but the optimizer is told to ignore it. Letting it use the index again is instant; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Mysql 10.11.6-MariaDB widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Sqlite 16.0 held: refused: ix_orders_status already has these keys and includes total.
Sqlite 16.0 add: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 16.0 unique: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 16.0 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Sqlite 16.0 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Sqlite 16.0 off: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 16.0 off-held: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. | cost: 
Sqlite 16.0 off-pk one: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 16.0 off-pk two: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Sqlite 16.0 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Sqlite 11.4 held: refused: ix_orders_status already has these keys and includes total.
Sqlite 11.4 add: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 11.4 unique: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 11.4 clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Sqlite 11.4 pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Sqlite 11.4 off: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 11.4 off-held: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. | cost: 
Sqlite 11.4 off-pk one: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 11.4 off-pk two: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Sqlite 11.4 widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
Sqlite 10.11.6-MariaDB held: refused: ix_orders_status already has these keys and includes total.
Sqlite 10.11.6-MariaDB add: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 10.11.6-MariaDB unique: refused: SQLite has no included columns: an index there holds only its keys.
Sqlite 10.11.6-MariaDB clustered: refused: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it.
Sqlite 10.11.6-MariaDB pk: refused: ix_orders_status enforces the primary key, and SQL Server won't change a constraint's index to include columns.
Sqlite 10.11.6-MariaDB off: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 10.11.6-MariaDB off-held: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. | cost: 
Sqlite 10.11.6-MariaDB off-pk one: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at; once it's back, the optimizer says whether it still wants it. | cost: 
Sqlite 10.11.6-MariaDB off-pk two: sql: ix_orders_status has these keys, but rebuilding it is all SQLite offers; if it was set aside because nothing read it, the queries below now would. It doesn't include created_at and channel; once it's back, the optimizer says whether it still wants them. | cost: 
Sqlite 10.11.6-MariaDB widen_fix: ix_orders_status is the table's clustered index: it already holds every column, so there is nothing to include in it. | None
week none: Some("shop.customers received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.")
week none, any: Some("shop.customers received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.")
week some: Some("shop.customers receives about 5.0K writes a day; each one will also update this index.")
week one: Some("shop.customers receives about 0.1 writes a day; each one will also update this index.")
short none: Some("shop.customers had no writes in the 10 h its counters cover, too short to say it gets none (a weekly job may not have run yet); each write it gets will also update this index.")
short some: Some("shop.customers receives 700 writes in the 10 hours since the server last restarted; each one will also update this index.")
reset none: Some("shop.orders had no writes in what its counters cover, which may be short: performance_schema's counters start over when they are reset, and a weekly job may not have run since; each write it gets will also update this index.")"#;
