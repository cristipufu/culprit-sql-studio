use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
fn extend(row: &Rec, fields: &[(&str, Val)]) -> Rec {
    Rec::of(&row.columns().map(|(key, value)| (key, value.clone())).chain(fields.iter().cloned()).collect::<Vec<_>>())
}
fn owner() -> instance::Context {
    instance::observation(&crate::processors::tests::row())
        .unwrap()
        .context_from_epoch(&crate::processors::tests::row())
        .unwrap()
        .unwrap()
}
pub(crate) fn pool_row() -> Rec {
    extend(&crate::processors::tests::row(),&[("pool_cpu_rows",Val::Text(r#"[{"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":"9007199254740993"},{"pool_id":2,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":"0"}]"#.into()))])
}
pub(crate) fn history_row() -> Rec {
    history("Windows", 10, 50)
}
fn history(platform: &str, process: i32, idle: i32) -> Rec {
    let record = format!(
        r#"<Record id="7"><SchedulerMonitorEvent><SystemHealth><ProcessUtilization>{process}</ProcessUtilization><SystemIdle>{idle}</SystemIdle><Unrelated>private-payload</Unrelated></SystemHealth></SchedulerMonitorEvent></Record>"#
    );
    extend(
        &crate::processors::tests::row(),
        &[
            ("record_ms_ticks", Val::Int(400)),
            ("host_platform", Val::Text(platform.into())),
            ("record_bytes", Val::Int(record.encode_utf16().count() as i64 * 2)),
            ("cpu_record", Val::Text(record)),
        ],
    )
}
#[tokio::test]
async fn pool_cpu_keeps_every_native_pool_reset_and_exact_total_in_one_bounded_original_read() {
    let mut reader = FakeRead::new(vec![Ok(vec![pool_row()])]);
    let rows = read_pools(&mut reader, &owner(), MAX_MSSQL_CPU_POOLS as usize).await.unwrap();
    assert_eq!(rows.len(), 2);
    let first = MssqlCpuPoolPoint::from_sample(&rows[0]).unwrap();
    assert_eq!((first.pool_id(), first.total_ms(), first.host_processors()), ("1", Some("9007199254740993"), "64"));
    assert_eq!(MssqlCpuPoolPoint::from_sample(&rows[1]).unwrap().total_ms(), Some("0"));
    assert!(rows.iter().all(|row| row.counters[0].reset == ResetMarker::Unknown));
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, 2);
    assert!(matches!(reader.calls[0].parameters.as_slice(), [Parameter::Integer(65)]));
    let sql = &reader.calls[0].sql;
    assert!(
        sql.contains("sys.dm_resource_governor_resource_pools")
            && sql.contains("statistics_start_time")
            && sql.contains("i.cpu_count AS host_processors")
    );
    assert!(!sql.contains("SUM(") && !sql.contains("workload_groups") && !sql.contains("WAITFOR"));
}
#[tokio::test]
async fn malformed_or_incomplete_pool_population_never_becomes_a_smaller_complete_cpu_sum() {
    for raw in [
        "[]",
        r#"[{"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003"}]"#,
        r#"[{"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":"-1"}]"#,
        r#"[{"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":1.5}]"#,
    ] {
        let mut reader =
            FakeRead::new(vec![Ok(vec![replace(&pool_row(), &[("pool_cpu_rows", Val::Text(raw.into()))])])]);
        assert!(read_pools(&mut reader, &owner(), 64).await.is_err(), "{raw}");
        assert_eq!(reader.calls.len(), 1);
    }
    for size in [2, 65] {
        let mut raw = vec![
            serde_json::json!({"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":"1"});
            size
        ];
        if size == 65 {
            for (index, row) in raw.iter_mut().enumerate() {
                row["pool_id"] = serde_json::json!(index + 1);
            }
        }
        let mut reader = FakeRead::new(vec![Ok(vec![replace(
            &pool_row(),
            &[("pool_cpu_rows", Val::Text(serde_json::to_string(&raw).unwrap()))],
        )])]);
        assert!(read_pools(&mut reader, &owner(), 64).await.is_err());
    }
    let mut reader = FakeRead::new(vec![]);
    assert!(read_pools(&mut reader, &owner(), 63).await.is_err());
    assert!(reader.calls.is_empty());
}
#[tokio::test]
async fn native_null_pool_total_is_unknown_and_foreign_process_refuses_before_returning_rows() {
    let raw =
        r#"[{"pool_id":1,"statistics_started_without_offset":"2020-01-01T05:00:00.003","total_cpu_usage_ms":null}]"#;
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&pool_row(), &[("pool_cpu_rows", Val::Text(raw.into()))])])]);
    let rows = read_pools(&mut reader, &owner(), 64).await.unwrap();
    assert_eq!(MssqlCpuPoolPoint::from_sample(&rows[0]).unwrap().total_ms(), None);
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&pool_row(), &[("process_id", Val::Int(43))])])]);
    assert!(read_pools(&mut reader, &owner(), 64).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}
#[tokio::test]
async fn documented_ring_fields_keep_native_sample_ticks_and_never_store_xml_or_other_cpu() {
    for (platform, idle) in [("Windows", 50), ("Linux", 0)] {
        let mut reader = FakeRead::new(vec![Ok(vec![history(platform, 10, idle)])]);
        let rows = read_history(&mut reader, &owner(), 1).await.unwrap();
        let point = MssqlCpuHistoryPoint::from_sample(&rows[0]).unwrap();
        assert_eq!(point.record_ticks(), "400");
        assert_eq!(point.process_percent(), Some("10"));
        assert_eq!(point.idle_percent(), (platform == "Windows").then_some("50"));
        assert_eq!(point.raw_idle(), Some(if platform == "Windows" { "50" } else { "0" }));
        let text = serde_json::to_string(&rows[0]).unwrap();
        assert!(!text.contains("private-payload") && !text.contains("SystemHealth") && !text.contains("other_cpu"));
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.calls[0].limit, 2);
        assert!(reader.calls[0].sql.contains("sys.dm_os_host_info") && !reader.calls[0].sql.contains("CONVERT(XML"));
    }
}
#[tokio::test]
async fn malformed_oversized_ambiguous_or_old_ring_sample_withholds_native_percent_without_fallback() {
    for change in [
        ("record_bytes", Val::Int(MAX_RING_RECORD_BYTES as i64 + 2)),
        ("record_ms_ticks", Val::Int(99)),
        ("record_ms_ticks", Val::Int(501)),
        ("cpu_record", Val::Text("<Record>".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&history_row(), &[change])])]);
        assert!(read_history(&mut reader, &owner(), 1).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let row = history_row();
    let mut reader = FakeRead::new(vec![Ok(vec![row.clone(), row])]);
    assert!(read_history(&mut reader, &owner(), 1).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}
#[tokio::test]
async fn unsupported_ring_percentage_is_retained_only_as_raw_diagnostic_and_query_debt_never_retries() {
    let mut reader = FakeRead::new(vec![Ok(vec![history("Unknown", 180, 0)])]);
    let rows = read_history(&mut reader, &owner(), 1).await.unwrap();
    let point = MssqlCpuHistoryPoint::from_sample(&rows[0]).unwrap();
    assert_eq!(point.raw_process(), Some("180"));
    assert_eq!(point.process_percent(), None);
    assert_eq!(point.idle_percent(), None);
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
    assert!(read_history(&mut reader, &owner(), 1).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}

#[test]
fn concrete_native_cpu_helpers_keep_reader_future_and_output_storage_bounded() {
    fn future_size<Args, Future>(_: impl FnOnce(Args) -> Future) -> usize {
        std::mem::size_of::<Future>()
    }
    let pool = future_size(|(reader, owner): (&'static mut crate::read::ReadSession, &'static instance::Context)| {
        read_pools(reader, owner, 64)
    });
    let history =
        future_size(|(reader, owner): (&'static mut crate::read::ReadSession, &'static instance::Context)| {
            read_history(reader, owner, 1)
        });
    assert!(pool <= 4096, "native pool CPU future {pool}");
    assert!(history <= 4096, "native CPU history future {history}");
    assert!(std::mem::size_of::<Vec<NativeCounterSet>>() <= 4096);
}
