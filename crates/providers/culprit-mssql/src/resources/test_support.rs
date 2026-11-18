use crate::{
    read::{NativeRead, Parameter, ReadResult},
    value::{Rec, Val},
};
use culprit_core::telemetry::SourceShutdown;
use std::collections::VecDeque;
pub(crate) struct Call {
    pub sql: String,
    pub parameters: Vec<Parameter>,
    pub limit: usize,
}
pub(crate) struct FakeRead {
    replies: VecDeque<ReadResult<Vec<Rec>>>,
    pub calls: Vec<Call>,
    pub closed: u32,
    pub shutdown: SourceShutdown,
    pause_at: Option<usize>,
    query_limit: Option<u32>,
}
impl FakeRead {
    pub(crate) fn new(replies: Vec<ReadResult<Vec<Rec>>>) -> Self {
        Self {
            replies: replies.into(),
            calls: Vec::new(),
            closed: 0,
            shutdown: SourceShutdown::Quiescent,
            pause_at: None,
            query_limit: None,
        }
    }
    pub(crate) fn pause_on_query(mut self, position: usize) -> Self {
        assert!(position > 0);
        self.pause_at = Some(position);
        self
    }
    pub(crate) fn with_query_limit(mut self, limit: u32) -> Self {
        self.query_limit = Some(limit);
        self
    }
}
#[async_trait::async_trait]
impl NativeRead for FakeRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        if self.query_limit.is_some_and(|limit| self.calls.len() >= limit as usize) {
            return Err(crate::read::ReadError::Stopped(culprit_core::telemetry::CollectionStop::QueryLimit));
        }
        self.calls.push(Call { sql: sql.to_owned(), parameters, limit: row_limit });
        if self.pause_at == Some(self.calls.len()) {
            std::future::pending::<()>().await;
        }
        self.replies.pop_front().expect("unexpected native resource read")
    }
    fn queries(&self) -> u32 {
        self.calls.len() as u32
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed += 1;
        self.shutdown
    }
}
pub(crate) fn replace(row: &Rec, values: &[(&str, Val)]) -> Rec {
    Rec::of(
        &row.columns()
            .map(|(name, value)| {
                (name, values.iter().find(|(key, _)| *key == name).map(|(_, value)| value).unwrap_or(value).clone())
            })
            .collect::<Vec<_>>(),
    )
}
pub(crate) fn row(at: &str) -> Rec {
    Rec::of(&[
        ("sample_at", Val::Text(at.into())),
        ("engine_edition", Val::Int(5)),
        ("database_edition", Val::Text("General Purpose".into())),
        ("replica_id", Val::Null),
        ("replica_role", Val::Int(0)),
        ("avg_cpu_percent", Val::Float(0.0)),
        ("avg_data_io_percent", Val::Float(20.0)),
        ("avg_log_write_percent", Val::Float(90.0)),
        ("avg_memory_usage_percent", Val::Null),
        ("max_worker_percent", Val::Float(55.0)),
        ("max_session_percent", Val::Float(30.0)),
        ("xtp_storage_percent", Val::Float(0.0)),
        ("avg_instance_cpu_percent", Val::Float(70.0)),
        ("avg_instance_memory_percent", Val::Float(40.0)),
        ("cpu_limit", Val::Float(10.0)),
        ("dtu_limit", Val::Null),
    ])
}

pub(crate) fn latest_row(at: &str) -> Rec {
    let sample = row(at);
    let mut fields = sample.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    fields.push(("observed_at", Val::Text("2020-01-01T06:02:00Z".into())));
    Rec::of(&fields)
}
pub(crate) fn inventory_row() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ("available_from", Val::Text("2020-01-01T05:00:00Z".into())),
        ("available_until", Val::Text("2020-01-01T06:02:00Z".into())),
        ("selected_count", Val::Int(2)),
        ("retained_count", Val::Int(240)),
        ("replica_role", Val::Int(0)),
        ("selected_first", Val::Text("2020-01-01T06:00:15Z".into())),
        ("selected_last", Val::Text("2020-01-01T06:00:30Z".into())),
    ])
}
pub(crate) fn window() -> super::Window {
    super::Window::new("2020-01-01T06:00:00Z".parse().unwrap(), "2020-01-01T06:01:00Z".parse().unwrap()).unwrap()
}

/// One explicit native instance-pressure point; shared by helper and streamed-source behavioral controls.
pub(crate) fn instance_pressure_row() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text("2026-11-18T12:00:00.0000000Z".into())),
        ("scheduler_population_rows", Val::Int(2)),
        ("scheduler_rows", Val::Int(2)),
        ("scheduler_invalid", Val::Int(0)),
        ("runnable_tasks", Val::Int(0)),
        ("worker_queue", Val::Int(0)),
        ("failed_worker_creations", Val::Int(0)),
        ("process_memory_rows", Val::Int(1)),
        ("process_memory_invalid", Val::Int(0)),
        ("process_physical_low", Val::Int(0)),
        ("process_virtual_low", Val::Int(0)),
        ("process_physical_used_kib", Val::Int(32768)),
        ("process_virtual_committed_kib", Val::Int(65536)),
        ("process_virtual_available_kib", Val::Int(131072)),
        ("system_memory_rows", Val::Int(1)),
        ("system_memory_invalid", Val::Int(0)),
        ("system_memory_low", Val::Int(0)),
        ("system_total_kib", Val::Int(1048576)),
        ("system_available_kib", Val::Int(524288)),
        ("grant_rows", Val::Int(0)),
        ("grant_waiters", Val::Int(0)),
        ("pending_io_rows", Val::Int(0)),
        ("pending_io_invalid", Val::Int(0)),
        ("os_pending_io", Val::Int(0)),
        ("completion_pending_io", Val::Int(0)),
        ("io_waiter_population_rows", Val::Int(0)),
        ("io_waiter_rows", Val::Int(0)),
        ("io_waiter_unknown_types", Val::Int(0)),
        ("io_waiter_unknown_durations", Val::Int(0)),
        ("longest_io_related_wait_ms", Val::Null),
    ])
}
