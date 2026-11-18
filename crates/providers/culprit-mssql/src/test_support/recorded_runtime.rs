//! Passive inspection of the original, fully drained shared Query Store summary. No additional native query.
use crate::{
    query_store::runtime::PlanSummary,
    read::{NativeRead, Parameter, ReadResult},
    value::Rec,
};
use culprit_core::telemetry::SourceShutdown;
use std::sync::{Arc, Mutex};

pub(crate) struct RecordedRuntime<R> {
    io: R,
    rows: Arc<Mutex<Vec<PlanSummary>>>,
}
impl<R> RecordedRuntime<R> {
    pub fn new(io: R) -> Self {
        Self { io, rows: Default::default() }
    }
    pub fn rows(&self) -> Arc<Mutex<Vec<PlanSummary>>> {
        self.rows.clone()
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> NativeRead for RecordedRuntime<R> {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        let rows = self.io.query(sql, parameters, limit).await?;
        if sql.contains(" AS known_executions") {
            *self.rows.lock().unwrap() =
                rows.iter().map(PlanSummary::decode).collect::<culprit_core::Result<_>>().unwrap();
        }
        Ok(rows)
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.io.connection_generation()
    }
    fn read_metrics(&self) -> Option<culprit_core::telemetry::ReadWorkMetrics> {
        self.io.read_metrics()
    }
    fn observe_session_work(&mut self, observation: crate::read::session_work::Observation) {
        self.io.observe_session_work(observation)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

pub(crate) fn same_duration(observed_ms: f64, original_us: f64) {
    let expected = crate::query_store::runtime::milliseconds(original_us);
    assert!(
        (observed_ms - expected).abs() <= 1e-9_f64.max(expected.abs() * 1e-12),
        "{observed_ms}ms differs from original{original_us}us"
    );
}
