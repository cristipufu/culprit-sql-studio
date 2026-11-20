use super::*;
use crate::value::Val;

pub(crate) fn status_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.200Z".into())),
        ("enabled", Val::Int(1)),
        ("has_permission", Val::Int(1)),
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
        ("scan_phase", Val::Text("Aggregate".into())),
        ("duration", Val::Int(0)),
        ("error_count", Val::Int(1)),
        ("tran_count", Val::Int(2)),
        ("log_record_count", Val::Int(8)),
        ("command_count", Val::Null),
        ("failed_sessions_count", Val::Int(1)),
        ("latency", Val::Int(0)),
        ("last_commit", Val::Null),
        ("last_cdc_commit", Val::Null),
        ("start_lsn", Val::Text("0".into())),
        ("current_lsn", Val::Text("0".into())),
        ("end_lsn", Val::Null),
    ])
}
pub(crate) fn error_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("observed_at", Val::Text("2020-01-01T06:00:00.600Z".into())),
        ("session_id", Val::Int(7)),
        ("phase_number", Val::Int(2)),
        ("entered", Val::Text("2019-12-31T23:00:00.999".into())),
        ("error_number", Val::Int(2628)),
        ("error_severity", Val::Int(16)),
        ("error_state", Val::Int(1)),
        ("start_lsn", Val::Text("00000016:00000058:0001".into())),
        ("begin_lsn", Val::Null),
        ("sequence_value", Val::Text("0".into())),
    ])
}
#[test]
fn sql_reads_only_documented_bounded_native_columns_without_private_error_payload_or_sort() {
    for sql in [sql::status(), sql::SCANS.into(), sql::ERRORS.into()] {
        assert!(sql.contains("/* culprit */"));
        assert!(!sql.contains("error_message"));
        for forbidden in
            ["sp_cdc_enable", "sp_cdc_scan", "sp_start_job", "ORDER BY", "SELECT *", "fn_cdc_get", "cdc.change_tables"]
        {
            assert!(!sql.contains(forbidden), "{forbidden}");
        }
    }
    assert!(sql::SCANS.contains("SELECT TOP (@P1)"));
    assert!(sql::ERRORS.contains("SELECT TOP (@P1)"));
    assert!(sql::SCANS.contains("FROM sys.dm_cdc_log_scan_sessions"));
    assert!(sql::ERRORS.contains("FROM sys.dm_cdc_errors"));
    assert!(sql::status().contains("@P1 nvarchar(128)"));
}
#[tokio::test]
async fn null_sentinels_and_native_local_values_survive_without_utc_or_sql_reconstruction() {
    let mut io = crate::resources::test_support::FakeRead::new(vec![Ok(vec![scan_row()]), Ok(vec![error_row()])]);
    let scan = scans(&mut io, 5, 33).await.unwrap().remove(0);
    let MssqlCdcData::ScanSession(scan) = scan.data else { unreachable!() };
    assert_eq!(scan.session_id, 0);
    assert_eq!(scan.duration_seconds.as_deref(), Some("0"));
    assert_eq!(scan.commands, None);
    assert_eq!(scan.started_without_offset.as_deref(), Some("2019-12-31T23:00:00.0000000"));
    assert_eq!(scan.ended_without_offset, None);
    assert_eq!(scan.current_lsn.as_deref(), Some("0"));
    let error = errors(&mut io, 5, 64).await.unwrap().remove(0);
    let MssqlCdcData::Error(error) = error.data else { unreachable!() };
    assert_eq!(error.error_number, 2628);
    assert_eq!(error.entered_without_offset.as_deref(), Some("2019-12-31T23:00:00.9990000"));
    assert_eq!(error.begin_lsn, None);
}
#[tokio::test]
async fn native_fixed_width_lsn_terminators_are_normalized_for_scan_and_error_rows() {
    let lsn = "0000002B:000001A8:0001";
    let terminated = format!("{lsn}\0");
    let scan = crate::resources::test_support::replace(
        &scan_row(),
        &[
            ("start_lsn", Val::Text(terminated.clone())),
            ("current_lsn", Val::Text("0\0".into())),
            ("end_lsn", Val::Null),
        ],
    );
    let error = crate::resources::test_support::replace(
        &error_row(),
        &[
            ("start_lsn", Val::Text(terminated.clone())),
            ("begin_lsn", Val::Null),
            ("sequence_value", Val::Text(terminated)),
        ],
    );
    let mut io = crate::resources::test_support::FakeRead::new(vec![Ok(vec![scan]), Ok(vec![error])]);
    let scan = scans(&mut io, 5, 33).await.unwrap().remove(0);
    let MssqlCdcData::ScanSession(scan) = scan.data else { unreachable!() };
    assert_eq!(scan.start_lsn.as_deref(), Some(lsn));
    assert_eq!(scan.current_lsn.as_deref(), Some("0"));
    assert_eq!(scan.end_lsn, None);
    let error = errors(&mut io, 5, 64).await.unwrap().remove(0);
    let MssqlCdcData::Error(error) = error.data else { unreachable!() };
    assert_eq!(error.start_lsn.as_deref(), Some(lsn));
    assert_eq!(error.begin_lsn, None);
    assert_eq!(error.sequence_value.as_deref(), Some(lsn));
}

#[tokio::test]
async fn lsn_normalization_never_hides_embedded_repeated_or_other_invalid_native_bytes() {
    for value in ["0000002B\0:000001A8:0001", "0000002B:000001A8:0001\0\0", "0000002B:000001A8:0001 ", "\0"] {
        let scan = crate::resources::test_support::replace(&scan_row(), &[("start_lsn", Val::Text(value.into()))]);
        let error =
            crate::resources::test_support::replace(&error_row(), &[("sequence_value", Val::Text(value.into()))]);
        let mut io = crate::resources::test_support::FakeRead::new(vec![Ok(vec![scan]), Ok(vec![error])]);
        assert!(scans(&mut io, 5, 33).await.is_err(), "{value:?}");
        assert!(errors(&mut io, 5, 64).await.is_err(), "{value:?}");
    }
}

#[tokio::test]
async fn wrong_database_negative_counter_and_malformed_native_time_withdraw_the_population() {
    for (key, value) in [
        ("database_id", Val::Int(1)),
        ("duration", Val::Int(-1)),
        ("started", Val::Text("unknown".into())),
        ("start_lsn", Val::Text("secret SQL value".into())),
    ] {
        let changed = crate::resources::test_support::replace(&scan_row(), &[(key, value)]);
        let mut io = crate::resources::test_support::FakeRead::new(vec![Ok(vec![changed])]);
        assert!(scans(&mut io, 5, 33).await.is_err(), "{key}");
        assert_eq!(io.queries(), 1);
    }
}
