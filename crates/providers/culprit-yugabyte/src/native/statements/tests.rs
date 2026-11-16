use super::*;
use crate::probe::Probe;

const NODE: &str = "0123456789abcdef0123456789abcdef";

fn metadata() -> Metadata {
    Metadata {
        probe: Probe {
            now: "2026-11-16T10:00:00Z".into(),
            started: Some("2026-11-15T00:00:00Z".into()),
            version: "2025.2.0.0".into(),
            compatibility: 150002,
            database: "shop".into(),
            oid: 16384,
            node: Some(NODE.into()),
            cluster: false,
            global: false,
            statements: true,
            statement_schema: Some("public".into()),
            statement_info: true,
            metrics: false,
            rpc_metrics: false,
            statement_role: None,
        },
        node: NODE.into(),
        backend: 42,
        database: culprit_core::DatabaseIdentity::server(Engine::Yugabyte, "shop", "fixture.invalid", 5433, None),
    }
}

fn layout(optional: &[&str]) -> Layout {
    Layout {
        columns: ["userid", "dbid", "queryid", "query", "calls", "rows", "total_exec_time"]
            .into_iter()
            .chain(optional.iter().copied())
            .map(str::to_owned)
            .collect(),
        function: "public.pg_stat_statements".into(),
        total: "total_exec_time",
    }
}

fn row(changes: &[(&str, Option<&str>)]) -> Row {
    let mut fields = BTreeMap::from([
        ("queryid", Some("-9223372036854775808")),
        ("dbid", Some("16384")),
        ("userid", Some("10")),
        ("toplevel", Some("true")),
        ("observed_at", Some("2026-11-16T10:00:00Z")),
        ("calls", Some("9007199254740993")),
        ("rows", Some("0")),
        ("total_exec_time", Some("25.5")),
        ("temp_blks_read", Some("18446744073709551615")),
        ("temp_blks_written", Some("9007199254740993")),
        ("temp_blk_read_time", Some("0")),
        ("temp_blk_write_time", None),
        ("temp_block_bytes", Some("8192")),
    ]);
    fields.extend(changes.iter().copied());
    Row::from_fields(fields.into_iter().map(|(name, value)| (name.into(), value.map(str::to_owned))))
}

#[test]
fn exact_native_points_and_scan_projection_share_block_work_without_a_runtime_or_interval_join() {
    let native = row(&[]);
    let layout = layout(&["toplevel", "temp_blks_read", "temp_blks_written", "temp_blk_read_time"]);
    let sample = counters(&native, &layout, &metadata()).unwrap();
    assert_eq!(sample.scope, MeasurementScope::Node);
    assert_eq!(sample.reset, ResetMarker::Unknown);
    assert_eq!(sample.identity.query_id, "-9223372036854775808");
    assert_eq!(sample.identity.qualifiers["node"], NODE);
    assert_eq!(sample.identity.qualifiers["userid"], "10");
    assert_eq!(sample.identity.qualifiers["database_oid"], "16384");
    assert_eq!(sample.identity.qualifiers["toplevel"], "true");
    let from_counters = statement_temporary_work(&sample, SOURCE).unwrap();
    assert_eq!(from_counters, vec![temporary_work(&native).unwrap().unwrap()]);
    let culprit_core::StatementTemporaryWork::BlockIo {
        bytes_per_block,
        read_blocks,
        written_blocks,
        read_time_ms,
        write_time_ms,
        ..
    } = &from_counters[0]
    else {
        panic!("native temporary block I/O")
    };
    assert_eq!(*bytes_per_block, Some(8192));
    assert_eq!(read_blocks.as_deref(), Some("18446744073709551615"));
    assert_eq!(written_blocks.as_deref(), Some("9007199254740993"));
    assert_eq!(*read_time_ms, Some(0.0));
    assert_eq!(*write_time_ms, None);
    assert!(
        !sample
            .totals
            .iter()
            .any(|metric| matches!(metric.key, RuntimeMetricKey::Native(ref name) if name.contains("shared_blk")))
    );
}

#[test]
fn detected_columns_use_the_existing_text_free_node_read_and_never_project_unsupported_timers() {
    let layout = layout(&["temp_blks_read", "temp_blks_written"]);
    let sql = layout.query(&metadata(), false, 6, 1024);
    assert!(sql.contains("public.pg_stat_statements(false) s WHERE s.dbid=16384 LIMIT 6"));
    assert!(sql.contains("s.temp_blks_read::text AS temp_blks_read"));
    assert!(sql.contains("NULL::text AS temp_blk_read_time"));
    assert!(sql.contains("s.queryid::text AS queryid"));
    let reads_query_text = sql.match_indices("s.query").any(|(offset, name)| {
        sql.as_bytes().get(offset + name.len()).is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
    });
    assert!(!reads_query_text, "counter reads must not project or inspect the native query-text column: {sql}");
    assert!(!sql.contains("s.temp_blk_read_time") && !sql.contains("gv$"));
    assert!(sql.contains("current_setting('block_size',true)"));
    let definitions = layout.query(&metadata(), true, 6, 1024);
    assert!(!definitions.contains("temp_blks") && !definitions.contains("block_size"));
}

#[test]
fn native_zero_null_and_unsupported_fields_remain_distinct() {
    let layout = layout(&[]);
    let empty = row(&[("temp_blks_read", None), ("temp_blks_written", None), ("temp_blk_read_time", None)]);
    let sample = counters(&empty, &layout, &metadata()).unwrap();
    assert!(statement_temporary_work(&sample, SOURCE).unwrap().is_empty());
    assert!(temporary_work(&empty).unwrap().is_none());
    let zero = row(&[("temp_blks_read", Some("0")), ("temp_blks_written", None), ("temp_blk_read_time", None)]);
    let work = temporary_work(&zero).unwrap().unwrap();
    assert!(work.has_measurements());
    assert!(!work.has_observed_work());
}

#[test]
fn malformed_or_missing_native_values_and_unqualified_or_foreign_identities_are_rejected() {
    let layout = layout(&[]);
    for (column, value) in [
        ("temp_blks_read", "-1"),
        ("temp_blks_written", "1.0"),
        ("temp_blks_read", "01"),
        ("temp_blks_written", "18446744073709551616"),
        ("temp_blk_read_time", "NaN"),
        ("temp_blk_write_time", "-1"),
        ("temp_block_bytes", "0"),
        ("temp_block_bytes", "4294967296"),
        ("dbid", "16385"),
        ("userid", "0"),
        ("queryid", "+1"),
        ("toplevel", "unknown"),
    ] {
        assert!(counters(&row(&[(column, Some(value))]), &layout, &metadata()).is_err(), "{column}={value}");
    }
    let mut missing = Vec::new();
    let native = row(&[]);
    for name in [
        "queryid",
        "dbid",
        "userid",
        "toplevel",
        "observed_at",
        "calls",
        "rows",
        "total_exec_time",
        "temp_blks_read",
        "temp_blks_written",
        "temp_blk_write_time",
        "temp_block_bytes",
    ] {
        missing.push((name.into(), native.text(name)));
    }
    assert!(counters(&Row::from_fields(missing), &layout, &metadata()).is_err());
    let mut unknown = metadata();
    unknown.node = "00000000000000000000000000000000".into();
    assert!(counters(&native, &layout, &unknown).is_err());
}

#[test]
fn same_query_on_different_nodes_users_or_top_level_contexts_keeps_distinct_native_keys() {
    let layout = layout(&[]);
    let original = counters(&row(&[]), &layout, &metadata()).unwrap();
    for change in ["node", "userid", "toplevel"] {
        let mut meta = metadata();
        let native = match change {
            "node" => {
                meta.node = "11111111111111111111111111111111".into();
                row(&[])
            }
            "userid" => row(&[("userid", Some("11"))]),
            _ => row(&[("toplevel", Some("false"))]),
        };
        let other = counters(&native, &layout, &meta).unwrap();
        assert_eq!(original.identity.query_id, other.identity.query_id);
        assert_ne!(original.key().unwrap(), other.key().unwrap(), "{change}");
    }
}

#[test]
fn optional_reset_visibility_changes_statement_source_consistency_without_changing_native_activity_identity() {
    let original = metadata();
    let mut changed = metadata();
    changed.probe.statement_info = false;
    assert!(!original.same_source(&changed));
    assert!(original.same_activity_source(&changed));
    assert_eq!(original.activity_epoch(), changed.activity_epoch());
}
