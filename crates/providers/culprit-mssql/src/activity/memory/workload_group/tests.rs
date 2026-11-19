use super::*;
use crate::{resources::test_support::replace, value::Val};

fn columns(stored: bool) -> Vec<Rec> {
    let group = names().map(|name| Rec::of(&[("name", Val::Text(inventory_name(name)))]));
    let limits =
        limit_names().filter(|_| stored).map(|name| Rec::of(&[("name", Val::Text(limit_inventory_name(name)))]));
    super::super::tests::columns().into_iter().chain(group).chain(limits).collect()
}

fn row() -> Rec {
    let original = super::super::tests::row();
    let mut columns = original.columns().map(|(name, value)| (name.to_owned(), value.clone())).collect::<Vec<_>>();
    for (name, value) in [
        ("group_id", Val::Int(7)),
        ("pool_id", Val::Int(2)),
        ("statistics_start_time", Val::Text("2020-01-01T05:59:00.003".into())),
        ("tempdb_data_space_kb", Val::Int(0)),
        ("peak_tempdb_data_space_kb", Val::Int(9_007_199_254_740_993)),
        ("total_tempdb_data_limit_violation_count", Val::Int(3)),
    ] {
        columns.push((alias(name), value));
    }
    for (name, value) in [
        ("group_id", Val::Int(7)),
        ("pool_id", Val::Int(2)),
        (LIMITS[0], Val::Float(0.5)),
        (LIMITS[1], Val::Float(10.25)),
    ] {
        columns.push((limit_alias(name), value));
    }
    Rec::of(&columns.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}

fn reader(major: i64, columns: Vec<Rec>, rows: Vec<Rec>) -> ActivityReader<crate::resources::test_support::FakeRead> {
    let platform = replace(&crate::instance::test_support::metadata(), &[("major", Val::Text(major.to_string()))]);
    let context = replace(&crate::activity::database_context::tests::row(3), &[("major", Val::Int(major))]);
    let rows = rows.iter().map(|row| replace(row, &[("edition", Val::Int(3)), ("major", Val::Int(major))])).collect();
    super::super::tests::reader(vec![
        Ok(vec![platform]),
        Ok(vec![crate::instance::test_support::epoch(100)]),
        Ok(vec![context]),
        Ok(columns),
        Ok(rows),
    ])
}

#[tokio::test]
async fn selected_original_group_keeps_exact_current_peak_rejections_and_stored_fractional_limits_in_one_read() {
    let mut reader = reader(17, columns(true), vec![row()]);
    let result = reader.observe_memory().await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let rows = result.unwrap().into_observations().unwrap();
    let group = rows[0].workload_group_tempdb.as_ref().unwrap();
    assert_eq!((&group.group_id, &group.pool_id), (&"7".to_owned(), &"2".to_owned()));
    assert_eq!(group.current_kibibytes.as_deref(), Some("0"));
    assert_eq!(group.peak_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(group.limit_violations.as_deref(), Some("3"));
    assert_eq!(group.statistics_started_without_offset.as_deref(), Some("2020-01-01T05:59:00.003"));
    assert_eq!(group.stored_limits.as_ref().unwrap().fixed_mebibytes.as_deref(), Some("0.5"));
    assert_eq!(group.stored_limits.as_ref().unwrap().percent.as_deref(), Some("10.25"));
    assert_eq!(reader.query_count(), 5);
    let sql = &reader.io.calls[4].sql;
    assert!(sql.contains("wg.group_id=mg.group_id AND wg.pool_id=mg.pool_id"));
    assert!(sql.contains("wc.group_id=wg.group_id AND wc.pool_id=wg.pool_id"));
    assert!(sql.contains("OPTION(MAXDOP 1, LOOP JOIN)"));
    assert!(!sql.contains("GROUP BY") && !sql.contains("ORDER BY"));
    assert!(reader.io.calls[3].sql.contains("VIEW ANY DEFINITION"));
    assert!(reader.io.calls[3].sql.contains("ProductMajorVersion'))>=17"));
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn sql_server_2022_and_unavailable_optional_group_layout_preserve_existing_memory_observations() {
    for (major, columns) in [
        (16, columns(true)),
        (17, super::super::tests::columns()),
        (
            17,
            columns(true)
                .into_iter()
                .filter(|row| row.str("name").as_deref() != Some("workload_group.tempdb_data_space_kb"))
                .collect(),
        ),
    ] {
        let mut reader = reader(major, columns, vec![row()]);
        let result = reader.observe_memory().await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        let rows = result.unwrap().into_observations().unwrap();
        assert!(rows[0].workload_group_tempdb.is_none());
        assert_eq!(rows[0].requested_kibibytes.as_deref(), Some("9007199254740993"));
        assert_eq!(reader.query_count(), 5);
        assert!(!reader.io.calls[4].sql.contains("JOIN sys.dm_resource_governor_workload_groups"));
    }
}

#[tokio::test]
async fn denied_stored_catalog_preserves_counters_and_native_null_limits_remain_distinct_from_unavailable() {
    for stored in [false, true] {
        let native = replace(
            &row(),
            &[(limit_alias(LIMITS[0]).as_str(), Val::Null), (limit_alias(LIMITS[1]).as_str(), Val::Null)],
        );
        let mut reader = reader(17, columns(stored), vec![native]);
        let result = reader.observe_memory().await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        let rows = result.unwrap().into_observations().unwrap();
        let group = rows[0].workload_group_tempdb.as_ref().unwrap();
        assert_eq!(group.limit_violations.as_deref(), Some("3"));
        assert_eq!(group.stored_limits.is_some(), stored);
        if let Some(limits) = &group.stored_limits {
            assert_eq!(limits.fixed_mebibytes, None);
            assert_eq!(limits.percent, None);
        }
    }
}

#[tokio::test]
async fn native_null_current_lifetime_peak_and_rejections_never_become_a_healthy_zero() {
    let changes = FIELDS.iter().map(|field| (alias(field.name), Val::Null)).collect::<Vec<_>>();
    let native =
        replace(&row(), &changes.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>());
    let mut reader = reader(17, columns(false), vec![native]);
    let result = reader.observe_memory().await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let rows = result.unwrap().into_observations().unwrap();
    let group = rows[0].workload_group_tempdb.as_ref().unwrap();
    assert_eq!(group.current_kibibytes, None);
    assert_eq!(group.peak_kibibytes, None);
    assert_eq!(group.limit_violations, None);
    assert_eq!(group.statistics_started_without_offset, None);
    for field in FIELDS {
        assert!(rows[0].native_null_columns.contains(&inventory_name(field.name)));
    }
}

#[tokio::test]
async fn wrong_group_pool_partial_keys_and_malformed_values_fail_without_fallback_or_borrowing_another_group() {
    for (name, value) in [
        (alias("group_id"), Val::Int(8)),
        (alias("pool_id"), Val::Int(3)),
        (alias("group_id"), Val::Null),
        (alias("tempdb_data_space_kb"), Val::Int(-1)),
        (alias("peak_tempdb_data_space_kb"), Val::Float(1.5)),
        (alias("statistics_start_time"), Val::Text("2020-01-01T05:59:00Z".into())),
        (limit_alias("pool_id"), Val::Int(3)),
        (limit_alias(LIMITS[1]), Val::Float(101.0)),
    ] {
        let mut reader = reader(17, columns(true), vec![replace(&row(), &[(name.as_str(), value)])]);
        let result = reader.observe_memory().await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert!(result.is_err(), "{name}");
        assert_eq!(reader.query_count(), 5);
    }
}

#[tokio::test]
async fn masked_group_ids_do_not_attach_shared_tempdb_context_or_drop_native_memory_evidence() {
    let native = replace(&row(), &[("grant_group_id", Val::Int(0)), (alias("group_id").as_str(), Val::Int(0))]);
    let mut reader = reader(17, columns(true), vec![native]);
    let result = reader.observe_memory().await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let rows = result.unwrap().into_observations().unwrap();
    assert!(rows[0].workload_group_tempdb.is_none());
    assert_eq!(rows[0].granted_kibibytes.as_deref(), Some("65536"));
}
