use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn availability_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.200Z".into())),
        ("has_database_state", Val::Int(1)),
        ("has_database_performance_state", Val::Int(0)),
        ("scan_layout", Val::Int(1)),
        ("error_layout", Val::Int(1)),
    ])
}
pub(crate) fn scan_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.400Z".into())),
        ("session_id", Val::Int(0)),
        ("started", Val::Text("2019-12-31T23:00:00".into())),
        ("ended", Val::Null),
        ("batch_processing_phase", Val::Text("Aggregate".into())),
        ("duration", Val::Int(0)),
        ("error_count", Val::Int(1)),
        ("batch_start_lsn", Val::Text("0".into())),
        ("currently_processed_lsn", Val::Text("0".into())),
        ("batch_end_lsn", Val::Null),
        ("tran_count", Val::Int(9007199254740993)),
        ("currently_processed_commit_lsn", Val::Null),
        ("processed_commit", Val::Null),
        ("log_record_count", Val::Int(8)),
        ("schema_change_count", Val::Int(0)),
        ("command_count", Val::Null),
        ("latency", Val::Int(3)),
        ("empty_scan_count", Val::Int(2)),
        ("failed_sessions_count", Val::Int(1)),
    ])
}
pub(crate) fn error_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.600Z".into())),
        ("session_id", Val::Int(0)),
        ("source_task", Val::Int(5)),
        ("table_id", Val::Null),
        ("table_group_id", Val::Null),
        ("capture_phase_number", Val::Null),
        ("entered", Val::Text("2019-12-31T23:00:00.999".into())),
        ("error_number", Val::Int(2628)),
        ("error_severity", Val::Int(16)),
        ("error_state", Val::Int(1)),
        ("batch_start_lsn", Val::Text("0".into())),
        ("batch_end_lsn", Val::Text("0".into())),
        ("tran_begin_lsn", Val::Null),
        ("tran_commit_lsn", Val::Null),
        ("sequence_value", Val::Text("0".into())),
        ("command_id", Val::Null),
    ])
}
#[test]
fn change_feed_sql_is_a_separate_bounded_read_only_native_projection_without_private_error_payload() {
    for query in [sql::availability(), sql::SCANS.into(), sql::ERRORS.into()] {
        assert!(query.contains("/* culprit */"));
        assert!(!query.contains("error_message"));
        for forbidden in ["ORDER BY", "SELECT *", "sp_change_feed", "sp_cdc", "sys.dm_cdc", "INSERT", "ALTER", "DBCC"] {
            assert!(!query.contains(forbidden), "{forbidden}");
        }
    }
    assert!(sql::SCANS.contains("TOP (@P1)"));
    assert!(sql::ERRORS.contains("TOP (@P1)"));
    assert!(sql::availability().contains("VIEW DATABASE STATE"));
    assert!(sql::availability().contains("VIEW DATABASE PERFORMANCE STATE"));
    assert!(sql::SCANS.contains("FROM sys.dm_change_feed_log_scan_sessions"));
    assert!(sql::ERRORS.contains("FROM sys.dm_change_feed_errors"));
}
#[tokio::test]
async fn native_null_zero_exact_counts_task_codes_and_local_clocks_survive_the_real_decoder() {
    let mut io = FakeRead::new(vec![Ok(vec![scan_row()]), Ok(vec![error_row()])]);
    let scan = scans(&mut io, 5, 33).await.unwrap().remove(0);
    let MssqlChangeFeedData::ScanSession(scan) = scan.data else { unreachable!() };
    assert_eq!(scan.session_id, 0);
    assert_eq!(scan.transactions.as_deref(), Some("9007199254740993"));
    assert_eq!(scan.duration_seconds.as_deref(), Some("0"));
    assert!(scan.ended_without_offset.is_none());
    assert_eq!(scan.started_without_offset.as_deref(), Some("2019-12-31T23:00:00.0000000"));
    let error = errors(&mut io, 5, 64).await.unwrap().remove(0);
    let MssqlChangeFeedData::Error(error) = error.data else { unreachable!() };
    assert_eq!(error.session_id, 0);
    assert_eq!(error.source_task, Some(5));
    assert!(error.table_id.is_none() && error.table_group_id.is_none() && error.command_id.is_none());
    assert_eq!(error.entered_without_offset.as_deref(), Some("2019-12-31T23:00:00.9990000"));
    assert_eq!(io.calls.len(), 2);
    assert_eq!(io.calls[0].limit, 34);
    assert_eq!(io.calls[1].limit, 65);
}
#[tokio::test]
async fn foreign_db_nonfinite_negative_and_invalid_native_table_context_refuse_instead_of_becoming_zero() {
    for (name, value) in [
        ("database_id", Val::Int(6)),
        ("source_task", Val::Int(256)),
        ("error_count", Val::Int(-1)),
        ("error_count", Val::Float(f64::NAN)),
        ("table_group_id", Val::Text("not-guid".into())),
    ] {
        let error_side = matches!(name, "source_task" | "table_group_id");
        let native = if error_side { error_row() } else { scan_row() };
        let mut io = FakeRead::new(vec![Ok(vec![replace(&native, &[(name, value)])])]);
        let result = if error_side { errors(&mut io, 5, 64).await } else { scans(&mut io, 5, 33).await };
        assert!(result.is_err(), "{name}");
        assert_eq!(io.calls.len(), 1);
    }
}
