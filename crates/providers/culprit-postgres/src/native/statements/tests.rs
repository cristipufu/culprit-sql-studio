use super::*;
use culprit_core::{DatabaseIdentity, MeasurementScope};
use std::collections::BTreeSet;

fn metadata(version: u32) -> Metadata {
    let mut columns: BTreeSet<String> = [
        "userid",
        "dbid",
        "queryid",
        "query",
        "calls",
        "rows",
        "total_exec_time",
        "mean_exec_time",
        "max_exec_time",
        "min_exec_time",
        "stddev_exec_time",
        "temp_blks_read",
        "temp_blks_written",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if version >= 140_000 {
        columns.insert("toplevel".into());
    }
    if version >= 150_000 {
        columns.extend([TEMP_READ_TIME.column.into(), TEMP_WRITE_TIME.column.into()]);
    }
    if version >= 170_000 {
        columns.extend(["stats_since".into(), "minmax_stats_since".into()]);
    }
    Metadata {
        database: DatabaseIdentity::server(Engine::Postgres, "app", "localhost", 5432, None),
        database_oid: "16384".into(),
        version,
        started_at: "2020-01-01T00:00:00Z".into(),
        at: "2020-01-02T00:00:00Z".into(),
        block_bytes: 16384,
        full_visibility: true,
        standby: false,
        statements: Some(StatementCatalog { schema: "public".into(), columns, info: true }),
    }
}

fn row() -> Row {
    fields(&[
        ("userid", Some("10")),
        ("dbid", Some("16384")),
        ("queryid", Some("-9223372036854775808")),
        ("toplevel", Some("true")),
        ("observed_at", Some("2020-01-02T00:00:00Z")),
        ("stats_since", Some("2020-01-01T12:00:00Z")),
        ("minmax_stats_since", Some("2020-01-01T18:00:00Z")),
        ("calls", Some("2")),
        ("rows", Some("1")),
        ("total_exec_time", Some("10")),
        ("mean_exec_time", Some("5")),
        ("min_exec_time", Some("1")),
        ("max_exec_time", Some("9")),
        ("stddev_exec_time", Some("4")),
        ("temp_blks_read", Some("9007199254740993")),
        ("temp_blks_written", Some("9223372036854775807")),
        ("temp_blk_read_time", Some("0")),
        ("temp_blk_write_time", Some("12.75")),
        ("temp_block_bytes", Some("16384")),
    ])
}
fn fields(values: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(values.iter().map(|(name, value)| (name.to_string(), value.map(str::to_owned))))
}

#[test]
fn native_and_scan_projection_share_exact_temporary_counts_units_and_timers() {
    let native = counters(&row(), &metadata(170_000)).unwrap();
    assert_eq!(native.scope, MeasurementScope::Database);
    assert_eq!(native.reset, ResetMarker::At("2020-01-01T12:00:00.000000000Z".into()));
    let expected = temporary_work(&row()).unwrap().unwrap();
    assert_eq!(statement_temporary_work(&native, "pg_stat_statements").unwrap(), vec![expected.clone()]);
    assert_eq!(
        expected,
        StatementTemporaryWork::BlockIo {
            source: "pg_stat_statements".into(),
            bytes_per_block: Some(16384),
            read_blocks: Some("9007199254740993".into()),
            written_blocks: Some("9223372036854775807".into()),
            read_time_ms: Some(0.0),
            write_time_ms: Some(12.75),
        }
    );
    for key in [
        RuntimeMetricKey::TemporaryBlocksRead,
        RuntimeMetricKey::TemporaryBlocksWritten,
        RuntimeMetricKey::TemporaryReadTime,
        RuntimeMetricKey::TemporaryWriteTime,
    ] {
        assert_eq!(native.totals.iter().filter(|metric| metric.key == key).count(), 1);
    }
    assert!(
        !native
            .totals
            .iter()
            .any(|metric| matches!(&metric.key, RuntimeMetricKey::Native(name) if name.contains("temporary")))
    );
}

#[test]
fn missing_block_size_remains_unknown_instead_of_using_a_default_page_size() {
    let work = temporary_work(&fields(&[("temp_blks_read", Some("1")), ("temp_blks_written", Some("0"))])).unwrap();
    assert!(matches!(work, Some(StatementTemporaryWork::BlockIo { bytes_per_block: None, .. })));
    assert!(temporary_work(&fields(&[])).unwrap().is_none());
    // sum(bigint) is numeric: a scan can retain a role-aggregate above one native bigint's maximum exactly.
    let aggregate = temporary_work(&fields(&[("temp_blks_written", Some("18446744073709551615"))])).unwrap();
    assert!(
        matches!(aggregate, Some(StatementTemporaryWork::BlockIo { written_blocks: Some(value), .. }) if value == "18446744073709551615")
    );
}

#[test]
fn native_zero_timers_stay_distinct_from_missing_or_unavailable_timing() {
    let work = temporary_work(&fields(&[
        ("temp_blks_read", Some("0")),
        ("temp_blk_read_time", Some("0")),
        ("temp_blk_write_time", None),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(
        work,
        StatementTemporaryWork::BlockIo {
            source: "pg_stat_statements".into(),
            bytes_per_block: None,
            read_blocks: Some("0".into()),
            written_blocks: None,
            read_time_ms: Some(0.0),
            write_time_ms: None,
        }
    );
    assert!(!work.has_observed_work(), "a native zero is not a positive observation or historical absence proof");
}

#[test]
fn extension_columns_qualify_timers_independently_of_server_version() {
    let mut meta = metadata(170_000);
    meta.statements.as_mut().unwrap().columns.remove(TEMP_WRITE_TIME.column);
    let layout = Layout::detect(meta.statements.as_ref().unwrap(), meta.version).unwrap();
    let projection = temporary_projection(&layout);
    assert!(projection.contains("sum(temp_blk_read_time) AS temp_blk_read_time"));
    assert!(projection.contains("NULL::float8 AS temp_blk_write_time"));
    let sql = query(&meta, false, 101, 1024).unwrap();
    assert!(sql.contains("s.temp_blk_read_time::text AS temp_blk_read_time"));
    assert!(!sql.contains("s.temp_blk_write_time"));
    let old = temporary_projection(&Layout::for_version(140_000));
    assert!(!old.contains("sum(temp_blk_"));
    assert!(old.contains("NULL::float8 AS temp_blk_read_time"));
    assert!(old.contains("NULL::float8 AS temp_blk_write_time"));
}

#[test]
fn scan_adds_native_columns_to_the_existing_database_aggregate_without_extra_reads_or_sorts() {
    let sql = statements_base("public.pg_stat_statements", 150_000);
    assert_eq!(sql.matches("FROM public.pg_stat_statements").count(), 1);
    assert!(sql.contains("sum(temp_blks_read)::text AS temp_blks_read"));
    assert!(sql.contains("sum(temp_blks_written)::text AS temp_blks_written"));
    assert!(sql.contains("sum(temp_blk_read_time) AS temp_blk_read_time"));
    assert!(sql.contains("sum(temp_blk_write_time) AS temp_blk_write_time"));
    assert!(sql.contains("current_setting('block_size', true)::integer AS temp_block_bytes"));
    assert!(sql.contains("dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) AND toplevel"));
    assert!(sql.contains("query NOT LIKE '%/* culprit */'"));
    assert!(!sql.contains("EXPLAIN") && !sql.contains("pg_stat_statements_reset"));
}

#[test]
fn native_capture_omits_text_and_keeps_database_bound_and_limit_with_typed_temp_metrics() {
    let sql = query(&metadata(170_000), false, 101, 1024).unwrap();
    assert!(sql.contains("public.pg_stat_statements(false) s WHERE s.dbid=16384 LIMIT 101"));
    assert!(!sql.contains("s.query AS") && !sql.contains("octet_length"));
    for name in ["temp_blks_read", "temp_blks_written", "temp_blk_read_time", "temp_blk_write_time"] {
        assert_eq!(sql.matches(&format!("s.{name}::text AS {name}")).count(), 1);
    }
}

#[test]
fn native_generations_keep_role_nesting_entry_and_extrema_resets_separate() {
    let meta = metadata(170_000);
    let sample = counters(&row(), &meta).unwrap();
    assert_eq!(sample.identity.qualifiers["userid"], "10");
    assert_eq!(sample.identity.qualifiers["dbid"], "16384");
    assert_eq!(sample.identity.qualifiers["toplevel"], "true");
    assert_eq!(sample.identity.qualifiers["postmaster_started_at"], meta.started_at);
    assert_eq!(sample.distributions[0].extrema_reset, ResetMarker::At("2020-01-01T18:00:00.000000000Z".into()));
    for (field, replacement) in [("userid", "11"), ("toplevel", "false"), ("stats_since", "2020-01-01T13:00:00Z")] {
        let mut values = row();
        let originals =
            ["userid", "dbid", "queryid", "toplevel", "observed_at", "stats_since", "calls", "rows", "total_exec_time"];
        values = Row::from_fields(
            originals
                .into_iter()
                .map(|name| (name.into(), if name == field { Some(replacement.into()) } else { values.text(name) })),
        );
        assert_ne!(counters(&values, &meta).unwrap().identity, sample.identity, "{field}");
    }
    let legacy = counters(
        &fields(&[
            ("userid", Some("10")),
            ("dbid", Some("16384")),
            ("queryid", Some("1")),
            ("observed_at", Some("2020-01-02T00:00:00Z")),
            ("calls", Some("1")),
            ("total_exec_time", Some("2")),
        ]),
        &metadata(140_000),
    )
    .unwrap();
    assert_eq!(legacy.reset, ResetMarker::Unknown);
}

#[test]
fn invalid_native_temp_values_fail_instead_of_disappearing_or_becoming_zero() {
    for (name, value) in [
        ("temp_blks_read", "-1"),
        ("temp_blks_written", "18446744073709551616"),
        ("temp_blk_read_time", "NaN"),
        ("temp_blk_write_time", "-0.5"),
        ("temp_block_bytes", "0"),
        ("temp_block_bytes", "123"),
    ] {
        let values = fields(&[("temp_blks_read", Some("1")), (name, Some(value))]);
        let values = if name == "temp_blks_read" { fields(&[(name, Some(value))]) } else { values };
        assert!(temporary_work(&values).is_err(), "{name}={value}");
    }
}
