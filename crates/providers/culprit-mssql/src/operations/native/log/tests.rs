use super::*;
use crate::{
    operations::native::test_support::{temp_log_layout_row, temp_log_row},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::telemetry::{OperationalFact, StoragePopulation};

#[tokio::test]
async fn shared_log_uses_the_original_bounded_projection_with_native_database_two_and_exact_quantities() {
    let mut io = FakeRead::new(vec![Ok(vec![temp_log_layout_row()]), Ok(vec![temp_log_row()])]);
    let rows = read(&mut io, Kind::TempLog, 1).await.unwrap().observations(Kind::TempLog).unwrap();
    let [row] = rows.as_slice() else { panic!("one exact shared log population") };
    let OperationalFact::Space(space) = &row.fact else { unreachable!() };
    assert_eq!(space.population, StoragePopulation::SharedTempdbTransactionLog);
    assert_eq!((space.kind, space.scope), (FileKind::Log, FileScope::Server));
    assert_eq!(space.database, None);
    assert_eq!(space.total_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(space.used_bytes.as_deref(), Some("0"));
    assert_eq!(space.limit_bytes, None);
    assert_eq!(space.growth_enabled, Some(true));
    assert_eq!(space.reuse_wait.as_deref(), Some("ACTIVE_TRANSACTION"));
    assert!(space.parts.is_empty());
    assert_eq!(row.source, crate::sources::LOG_CAPACITY.name);
    assert_eq!(row.key, "shared-tempdb-log");
    assert_eq!(io.calls.len(), 2);
    let sql = &io.calls[1].sql;
    assert!(sql.contains("FROM tempdb.sys.dm_db_log_space_usage"));
    assert!(sql.contains("database_id AS database_id") && sql.contains("WHERE database_id=2"));
    assert!(sql.contains("FROM tempdb.sys.database_files WHERE type=1"));
    assert!(sql.contains("OPTION(MAXDOP 1)") && sql.contains("/* culprit */"));
    for forbidden in ["sys.master_files", "dm_tran_", "dm_db_log_stats", "DBCC", "BACKUP", "USE "] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn ordinary_database_log_template_and_default_scan_population_remain_byte_for_byte_unchanged() {
    let sql = format!("{} OPTION(MAXDOP 1)", Kind::Log.sql());
    assert_eq!(
        culprit_core::telemetry::digest(sql.as_bytes()),
        "3768ff0c254c704159ec14523d9647155ccf4d57f842e09d884d252441b606f4"
    );
    assert!(!sql.contains("tempdb") && !sql.contains("database_id AS database_id"));
    assert!(!Kind::SCAN.contains(&Kind::TempLog));
}

#[tokio::test]
async fn known_unsupported_hidden_and_null_native_capabilities_fail_distinctly_without_a_fallback() {
    for (field, value, expected) in [
        ("edition", Val::Int(5), culprit_core::ErrorCode::Unsupported),
        ("edition", Val::Int(255), culprit_core::ErrorCode::Unsupported),
        ("major", Val::Int(10), culprit_core::ErrorCode::Unsupported),
        ("usage_columns", Val::Int(2), culprit_core::ErrorCode::Unsupported),
        ("file_columns", Val::Int(3), culprit_core::ErrorCode::Unsupported),
        ("reuse_columns", Val::Int(0), culprit_core::ErrorCode::Unsupported),
        ("metadata_visible", Val::Int(0), culprit_core::ErrorCode::PermissionDenied),
        ("log_permission", Val::Int(0), culprit_core::ErrorCode::PermissionDenied),
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&temp_log_layout_row(), &[(field, value)])])]);
        assert_eq!(read(&mut io, Kind::TempLog, 1).await.err().unwrap().code, expected, "{field}");
        assert_eq!(io.calls.len(), 1);
    }
    for field in ["usage_columns", "log_permission", "database_id"] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&temp_log_layout_row(), &[(field, Val::Null)])])]);
        assert!(read(&mut io, Kind::TempLog, 1).await.is_err(), "{field}");
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn actual_dmv_identity_is_required_even_when_the_targeted_catalog_and_view_names_match() {
    for value in [Val::Null, Val::Int(0), Val::Int(5)] {
        let row = replace(&temp_log_row(), &[("database_id", value)]);
        let mut io = FakeRead::new(vec![Ok(vec![temp_log_layout_row()]), Ok(vec![row])]);
        assert!(read(&mut io, Kind::TempLog, 1).await.is_err());
        assert_eq!(io.calls.len(), 2);
    }
    let mut io = FakeRead::new(vec![
        Ok(vec![temp_log_layout_row()]),
        Ok(vec![replace(&temp_log_row(), &[("database_name", Val::Text("sample".into()))])]),
    ]);
    assert_eq!(
        CollectionStop::from_error(&read(&mut io, Kind::TempLog, 1).await.err().unwrap()),
        CollectionStop::SourceChanged
    );
}

#[tokio::test]
async fn native_nulls_zeroes_and_configured_growth_values_are_retained_without_invented_log_headroom() {
    for (used, growth, limit, reason) in [
        (Val::Null, Val::Null, Val::Null, Val::Null),
        (Val::Int(0), Val::Int(0), Val::Int(9_007_199_254_740_993), Val::Text("NOTHING".into())),
    ] {
        let row = replace(
            &temp_log_row(),
            &[
                ("used", used.clone()),
                ("can_grow", growth.clone()),
                ("max_bytes", limit.clone()),
                ("reuse_wait", reason.clone()),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![temp_log_layout_row()]), Ok(vec![row])]);
        let Facts::Spaces(rows) = read(&mut io, Kind::TempLog, 1).await.unwrap() else { unreachable!() };
        assert_eq!(rows[0].used_bytes.is_none(), matches!(used, Val::Null));
        assert_eq!(rows[0].growth_enabled.is_none(), matches!(growth, Val::Null));
        assert_eq!(rows[0].limit_bytes.is_none(), matches!(limit, Val::Null));
        assert_eq!(rows[0].reuse_wait.is_none(), matches!(reason, Val::Null));
    }
}

#[tokio::test]
async fn missing_duplicate_malformed_and_cancelled_results_never_retry_or_publish_a_healthy_empty_log() {
    for rows in [
        vec![],
        vec![temp_log_row(), temp_log_row()],
        vec![replace(&temp_log_row(), &[("used", Val::Int(-1))])],
        vec![replace(&temp_log_row(), &[("total", Val::Float(9_007_199_254_740_992.0))])],
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![temp_log_layout_row()]), Ok(rows)]);
        assert!(read(&mut io, Kind::TempLog, 1).await.is_err());
        assert_eq!(io.calls.len(), 2);
    }
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut io = FakeRead::new(vec![Ok(vec![temp_log_layout_row()]), Err(ReadError::Stopped(stop))]);
        assert_eq!(CollectionStop::from_error(&read(&mut io, Kind::TempLog, 1).await.err().unwrap()), stop);
        assert_eq!(io.calls.len(), 2);
    }
    let mut io = FakeRead::new(vec![]);
    assert!(read(&mut io, Kind::TempLog, 0).await.is_err());
    assert!(io.calls.is_empty());
}
