use super::*;
use crate::{
    resources::test_support::{FakeRead as BufferedRead, replace},
    value::Val,
};

use crate::read::{ReadBatch, ReadResult};
use culprit_core::telemetry::SourceShutdown;
use std::collections::VecDeque;
pub(crate) struct StreamRead {
    pub buffered: BufferedRead,
    batches: VecDeque<ReadResult<ReadBatch>>,
    pub pause_batches: bool,
    pub fail_after_first: bool,
}
impl StreamRead {
    pub(crate) fn new(replies: Vec<ReadResult<Vec<Rec>>>) -> Self {
        Self {
            buffered: BufferedRead::new(replies),
            batches: VecDeque::new(),
            pause_batches: false,
            fail_after_first: false,
        }
    }
    pub(crate) fn with_query_limit(mut self, limit: u32) -> Self {
        self.buffered = self.buffered.with_query_limit(limit);
        self
    }
    pub(crate) fn pause_on_query(mut self, position: usize) -> Self {
        self.buffered = self.buffered.pause_on_query(position);
        self
    }
}
#[async_trait::async_trait]
impl NativeRead for StreamRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.buffered.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.buffered.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.buffered.close().await
    }
}
#[async_trait::async_trait]
impl NativeStreamRead for StreamRead {
    async fn begin_stream(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<()> {
        let rows = self.buffered.query(sql, parameters, limit).await?;
        self.batches = rows.chunks(8).map(|chunk| Ok(ReadBatch { rows: chunk.to_vec(), complete: false })).collect();
        if self.fail_after_first {
            self.batches.truncate(1);
            self.batches.push_back(Err(ReadError::Stopped(CollectionStop::TimedOut)));
        } else {
            self.batches.push_back(Ok(ReadBatch { rows: vec![], complete: true }));
        }
        Ok(())
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        if self.pause_batches {
            std::future::pending::<()>().await;
        }
        self.batches.pop_front().expect("one bounded native stream")
    }
}

pub(crate) fn metadata(at: &str) -> Rec {
    replace(
        &crate::instance::test_support::metadata(),
        &[("edition", Val::Int(8)), ("observed_at", Val::Text(at.into()))],
    )
}
pub(crate) fn layout_rows() -> Vec<Rec> {
    layout::Field::ALL
        .into_iter()
        .map(|field| {
            let (kind, length, precision, scale) = match field {
                layout::Field::Start => (42, 8, 27, 7),
                layout::Field::End => (61, 8, 23, 3),
                layout::Field::Cpu => (106, 5, 5, 2),
                layout::Field::Used => (106, 9, 18, 2),
                layout::Field::Cores => (56, 4, 10, 0),
                layout::Field::Reserved | layout::Field::Requests | layout::Field::Read | layout::Field::Written => {
                    (127, 8, 19, 0)
                }
                _ => (231, 256, 0, 0),
            };
            Rec::of(&[
                ("server_state", Val::Int(1)),
                ("server_performance_state", Val::Int(1)),
                ("native_object_id", Val::Int(99)),
                ("native_column", Val::Text(field.name().into())),
                ("native_type", Val::Int(kind)),
                ("native_length", Val::Int(length)),
                ("native_precision", Val::Int(precision)),
                ("native_scale", Val::Int(scale)),
            ])
        })
        .collect()
}
pub(crate) fn frame(start: &str, end: &str) -> Rec {
    let metadata = metadata("2020-01-01T06:10:00Z");
    let mut fields = metadata.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("started_at_ticks", Val::Int(100)),
        ("ms_ticks", Val::Int(600)),
        ("present", Val::Int(1)),
        ("start_time", Val::Text(start.into())),
        ("end_time", Val::Text(end.into())),
        ("avg_cpu_percent", Val::Text("12.34".into())),
        ("virtual_core_count", Val::Text("8".into())),
        ("reserved_storage_mb", Val::Text("1048576".into())),
        ("storage_space_used_mb", Val::Text("9007199254740992.25".into())),
        ("io_request", Val::Text("9007199254740993".into())),
        ("io_bytes_read", Val::Text("9223372036854775807".into())),
        ("io_bytes_written", Val::Text("0".into())),
        ("resource_type", Val::Text("managed_instance".into())),
        ("resource_name", Val::Text("native-private-name".into())),
        ("sku", Val::Text("General Purpose".into())),
        ("hardware_generation", Val::Null),
    ]);
    Rec::of(&fields)
}
pub(crate) fn first() -> Rec {
    frame("2020-01-01T06:00:00Z", "2020-01-01T06:00:15Z")
}
pub(crate) fn empty_frame() -> Rec {
    let fields = layout::Field::ALL
        .into_iter()
        .map(|field| (field.name(), Val::Null))
        .chain(std::iter::once(("present", Val::Null)))
        .collect::<Vec<_>>();
    replace(&first(), &fields)
}
fn owner() -> instance::Context {
    instance::observation(&metadata("2020-01-01T06:09:59Z"))
        .unwrap()
        .context_from_epoch(&crate::instance::test_support::epoch(100))
        .unwrap()
        .unwrap()
}
fn available() -> Layout {
    Layout::decode(&layout_rows(), 16).unwrap()
}
#[tokio::test]
async fn one_selected_window_query_preserves_native_interval_totals_and_exact_decimal_strings() {
    let mut reader = StreamRead::new(vec![Ok(vec![first()])]);
    let result = read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.unwrap();
    assert_eq!(result.native_rows, 1);
    assert!(!result.limited);
    assert_eq!(reader.buffered.calls.len(), 1);
    let native = result.records[0].mssql_managed_instance.as_ref().unwrap();
    assert_eq!(native.io_request.as_deref(), Some("9007199254740993"));
    assert_eq!(native.storage_space_used_mb.as_deref(), Some("9007199254740992.25"));
    assert_eq!(native.io_bytes_written.as_deref(), Some("0"));
    assert_eq!(result.records[0].metrics.len(), 1);
    assert_eq!(result.records[0].metrics[0].value, Some(12.34));
    assert_eq!(result.records[0].replica_id, None);
    let call = &reader.buffered.calls[0];
    assert_eq!(call.limit, 3);
    assert!(matches!(&call.parameters[0],Parameter::Text(value) if value=="2020-01-01T06:00:00+00:00"));
    assert!(matches!(call.parameters[2], Parameter::Integer(3)));
    assert!(call.sql.contains("TOP (@P3)"));
    assert!(call.sql.contains("master.sys.server_resource_stats"));
    assert!(call.sql.contains("ORDER BY end_time ASC"));
    assert_eq!(call.sql.matches(" AS observed_at").count(), 1);
    for forbidden in ["COUNT(", "MIN(", "MAX(", "USE ", "UPDATE ", "INSERT ", "sys.resource_stats"] {
        assert!(!call.sql.contains(forbidden), "{forbidden}");
    }
}
#[tokio::test]
async fn native_nulls_and_unsupported_columns_remain_independent_without_an_alternate_query() {
    let mut columns = layout_rows();
    columns.retain(|row| text(row, "native_column").unwrap() != "io_bytes_written");
    let layout = Layout::decode(&columns, 16).unwrap();
    assert!(layout.available());
    assert_eq!(layout.native.io_bytes_written, MssqlCatalogAvailability::Unsupported);
    let row = replace(
        &first(),
        &[("avg_cpu_percent", Val::Null), ("virtual_core_count", Val::Null), ("io_bytes_written", Val::Null)],
    );
    let mut reader = StreamRead::new(vec![Ok(vec![row])]);
    let result = read(&mut reader, &crate::resources::test_support::window(), &layout, 2, &owner()).await.unwrap();
    assert_eq!(result.records[0].metrics[0].value, None);
    assert_eq!(result.records[0].mssql_managed_instance.as_ref().unwrap().virtual_core_count, None);
    assert!(reader.buffered.calls[0].sql.contains("CAST(NULL AS nvarchar(256)) AS io_bytes_written"));
    assert!(!reader.buffered.calls[0].sql.contains("s.io_bytes_written"));
    assert_eq!(reader.buffered.calls.len(), 1);
}
#[test]
fn native_layout_permissions_visibility_and_sql_types_do_not_turn_missing_into_zero() {
    for (change, status, code) in [
        ("denied", MssqlCatalogAvailability::PermissionDenied, ErrorCode::PermissionDenied),
        ("missing", MssqlCatalogAvailability::Unknown, ErrorCode::QueryFailed),
        ("unsupported", MssqlCatalogAvailability::Unsupported, ErrorCode::Unsupported),
    ] {
        let rows = match change {
            "denied" => layout_rows().iter().map(|row| replace(row, &[("server_state", Val::Int(0))])).collect(),
            "missing" => vec![Rec::of(&[
                ("server_state", Val::Int(1)),
                ("server_performance_state", Val::Int(1)),
                ("native_object_id", Val::Null),
                ("native_column", Val::Null),
            ])],
            _ => layout_rows()
                .iter()
                .map(|row| {
                    if text(row, "native_column").unwrap() == "end_time" {
                        replace(row, &[("native_type", Val::Int(42))])
                    } else {
                        row.clone()
                    }
                })
                .collect(),
        };
        let layout = Layout::decode(&rows, 16).unwrap();
        assert!(!layout.available(), "{change}");
        assert_eq!(layout.native.interval, status, "{change}");
        assert_eq!(layout.unavailable_code(), code, "{change}");
    }
    let denied =
        layout_rows().iter().map(|row| replace(row, &[("server_performance_state", Val::Int(0))])).collect::<Vec<_>>();
    assert!(!Layout::decode(&denied, 16).unwrap().available());
    assert!(
        Layout::decode(&denied, 15).unwrap().available(),
        "the view requires server state, while the independent sys_info owner follows its versioned permission"
    );
}
#[test]
fn contradictory_or_duplicate_layout_rows_are_not_an_available_native_contract() {
    let mut duplicate = layout_rows();
    duplicate.push(duplicate[0].clone());
    assert!(Layout::decode(&duplicate, 16).is_err());
    let mut changed = layout_rows();
    changed[1] = replace(&changed[1], &[("native_object_id", Val::Int(100))]);
    assert!(Layout::decode(&changed, 16).is_err());
}
#[tokio::test]
async fn selected_prefix_validates_its_one_lookahead_before_retaining_a_partial_result() {
    let second = frame("2020-01-01T06:00:15Z", "2020-01-01T06:00:30Z");
    let mut reader = StreamRead::new(vec![Ok(vec![first(), second.clone()])]);
    let result = read(&mut reader, &crate::resources::test_support::window(), &available(), 1, &owner()).await.unwrap();
    assert!(result.limited);
    assert_eq!(result.native_rows, 2);
    assert_eq!(result.records.len(), 1);
    assert_eq!(reader.buffered.calls.len(), 1);
    let malformed = replace(&second, &[("io_request", Val::Text("-1".into()))]);
    let mut reader = StreamRead::new(vec![Ok(vec![first(), malformed])]);
    assert!(read(&mut reader, &crate::resources::test_support::window(), &available(), 1, &owner()).await.is_err());
}
#[tokio::test]
async fn current_observer_and_historical_row_refusals_never_trigger_another_read() {
    for (label, changes) in [
        ("process", vec![("process_id", Val::Int(43))]),
        ("epoch", vec![("started_at_ticks", Val::Int(101))]),
        ("member", vec![("server_name", Val::Text("other".into()))]),
        ("platform", vec![("edition", Val::Int(3))]),
        ("interval", vec![("start_time", Val::Text("2020-01-01T06:00:16Z".into()))]),
        ("outside", vec![("end_time", Val::Text("2020-01-01T06:01:00Z".into()))]),
        ("invalid-number", vec![("io_bytes_read", Val::Text("1.25".into()))]),
    ] {
        let mut reader = StreamRead::new(vec![Ok(vec![replace(&first(), &changes)])]);
        assert!(
            read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.is_err(),
            "{label}"
        );
        assert_eq!(reader.buffered.calls.len(), 1, "{label}");
    }
    for rows in [vec![first(), first()], vec![frame("2020-01-01T06:00:15Z", "2020-01-01T06:00:30Z"), first()]] {
        let mut reader = StreamRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.is_err());
    }
}
#[tokio::test]
async fn empty_native_frame_preserves_a_current_observer_but_never_manufactures_a_sample() {
    let mut reader = StreamRead::new(vec![Ok(vec![empty_frame()])]);
    let result = read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.unwrap();
    assert_eq!(result.native_rows, 0);
    assert!(result.records.is_empty());
    let contradictory = replace(&empty_frame(), &[("io_request", Val::Text("0".into()))]);
    let mut reader = StreamRead::new(vec![Ok(vec![contradictory])]);
    assert!(read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.is_err());
}

#[tokio::test]
async fn unsupported_cpu_column_does_not_hide_known_native_storage_or_interval_io() {
    let mut columns = layout_rows();
    columns.retain(|row| text(row, "native_column").unwrap() != "avg_cpu_percent");
    let layout = Layout::decode(&columns, 16).unwrap();
    assert!(layout.available());
    assert_eq!(layout.native.cpu, MssqlCatalogAvailability::Unsupported);
    let row = replace(&first(), &[("avg_cpu_percent", Val::Null)]);
    let mut reader = StreamRead::new(vec![Ok(vec![row])]);
    let read = read(&mut reader, &crate::resources::test_support::window(), &layout, 2, &owner()).await.unwrap();
    assert_eq!(read.records[0].metrics[0].value, None);
    assert_eq!(
        read.records[0].mssql_managed_instance.as_ref().unwrap().io_request.as_deref(),
        Some("9007199254740993")
    );
    assert!(reader.buffered.calls[0].sql.contains("CAST(NULL AS nvarchar(256)) AS avg_cpu_percent"));
    for field in ["start_time", "end_time"] {
        assert!(reader.buffered.calls[0].sql.contains(&format!("CAST(s.{field} AS datetime2(7))")));
        assert!(!reader.buffered.calls[0].sql.contains(&format!("CAST(NULL AS varchar(40)) AS {field}")));
    }
}

#[tokio::test]
async fn a_late_native_stream_failure_withholds_the_earlier_batch_without_a_fallback_read() {
    let mut reader = StreamRead::new(vec![Ok(vec![first()])]);
    reader.fail_after_first = true;
    let error =
        read(&mut reader, &crate::resources::test_support::window(), &available(), 2, &owner()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(reader.buffered.calls.len(), 1);
}
