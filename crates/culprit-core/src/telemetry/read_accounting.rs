//! Bounded, engine-aware accounting shared by owned native readers. No driver or scheduling dependencies.
use crate::{
    Engine,
    telemetry::{
        DefinitionPayload, MAX_READ_TEMPLATE_PREVIEW_BYTES, MAX_READ_TEMPLATE_TEXT_BYTES, MAX_READ_TEMPLATES,
        PayloadUnavailable, ReadQueryMetrics, ReadWorkMetrics, digest,
    },
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
// Leave room for every explicit unavailable state, including JSON escaping and enum metadata.
const UNAVAILABLE_PREVIEW_ALLOWANCE: usize = 128;
#[derive(Default)]
pub struct ReadProgress {
    pub rows: AtomicU64,
    pub bytes: AtomicU64,
    pub drained: std::sync::atomic::AtomicBool,
}
#[derive(Default)]
struct State {
    active: u32,
    untracked: u32,
    templates: BTreeMap<String, Accumulator>,
}
struct Accumulator {
    statement: DefinitionPayload,
    completed: u32,
    failures: u32,
    timeouts: u32,
    cancelled: u32,
    unconfirmed: u32,
    rows: u64,
    bytes: u64,
    elapsed_ms: f64,
}
#[derive(Clone)]
pub struct ReadMetricsRecorder {
    engine: Engine,
    state: Arc<Mutex<State>>,
}
impl ReadMetricsRecorder {
    pub fn new(engine: Engine) -> Self {
        Self { engine, state: Arc::new(Mutex::new(State::default())) }
    }

    pub fn begin(&self, sql: &str) -> Option<String> {
        let id = digest(sql.as_bytes());
        let mut state = self.state.lock().ok()?;
        state.active = state.active.saturating_add(1);
        if !state.templates.contains_key(&id) {
            if state.templates.len() == MAX_READ_TEMPLATES {
                state.untracked = state.untracked.saturating_add(1);
                return None;
            }
            let mut statement = if sql.len() <= MAX_READ_TEMPLATE_TEXT_BYTES {
                DefinitionPayload::sql(sql, self.engine)
            } else {
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
            };
            if matches!(&statement,DefinitionPayload::Available {text,..} if text.len()>MAX_READ_TEMPLATE_TEXT_BYTES) {
                statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
            }
            state.templates.insert(
                id.clone(),
                Accumulator {
                    statement,
                    completed: 0,
                    failures: 0,
                    timeouts: 0,
                    cancelled: 0,
                    unconfirmed: 0,
                    rows: 0,
                    bytes: 0,
                    elapsed_ms: 0.0,
                },
            );
        }
        Some(id)
    }
    pub fn finish(&self, id: Option<&str>, elapsed: Duration, progress: &ReadProgress, outcome: &ReadTaskOutcome) {
        let Ok(mut state) = self.state.lock() else { return };
        state.active = state.active.saturating_sub(1);
        let Some(metric) = id.and_then(|id| state.templates.get_mut(id)) else { return };
        metric.completed = metric.completed.saturating_add(1);
        metric.failures = metric.failures.saturating_add(u32::from(outcome.failed));
        metric.timeouts = metric.timeouts.saturating_add(u32::from(outcome.timed_out));
        metric.cancelled = metric.cancelled.saturating_add(u32::from(outcome.cancelled));
        metric.unconfirmed = metric.unconfirmed.saturating_add(u32::from(outcome.unconfirmed));
        metric.rows = metric.rows.saturating_add(progress.rows.load(Ordering::Relaxed));
        metric.bytes = metric.bytes.saturating_add(progress.bytes.load(Ordering::Relaxed));
        metric.elapsed_ms += elapsed.as_secs_f64() * 1000.0;
    }
    pub fn snapshot(&self) -> Option<ReadWorkMetrics> {
        let state = self.state.lock().ok()?;
        let mut preview_bytes = 0;
        let templates = state
            .templates
            .iter()
            .map(|(id, value)| {
                let mut statement = value.statement.clone();
                let size = serde_json::to_vec(&statement).ok().map(|bytes| bytes.len());
                match size {
                    Some(size)
                        if preview_bytes + size
                            <= MAX_READ_TEMPLATE_PREVIEW_BYTES - MAX_READ_TEMPLATES * UNAVAILABLE_PREVIEW_ALLOWANCE =>
                    {
                        preview_bytes += size
                    }
                    _ => statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized },
                }
                ReadQueryMetrics {
                    template_id: id.clone(),
                    statement,
                    completed_tasks: value.completed,
                    failures: value.failures,
                    timeouts: value.timeouts,
                    cancellations: value.cancelled,
                    unconfirmed_cancellations: value.unconfirmed,
                    rows_decoded: value.rows.to_string(),
                    charged_value_bytes: value.bytes.to_string(),
                    client_elapsed_ms: value.elapsed_ms,
                }
            })
            .collect();
        Some(ReadWorkMetrics { active_tasks: state.active, untracked_tasks: state.untracked, templates })
    }
}
#[derive(Default)]
pub struct ReadTaskOutcome {
    pub failed: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub unconfirmed: bool,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounting_keeps_a_fixed_template_budget_and_never_persists_private_literals() {
        let metrics = ReadMetricsRecorder::new(Engine::Mssql);
        let progress = ReadProgress::default();
        for number in 0..100 {
            let id = metrics.begin(&format!("SELECT N'private-metric-canary-{number}'"));
            metrics.finish(id.as_deref(), Duration::from_millis(2), &progress, &ReadTaskOutcome::default());
        }
        let snapshot = metrics.snapshot().unwrap();
        assert!(snapshot.is_valid());
        assert_eq!(snapshot.active_tasks, 0);
        assert_eq!(snapshot.templates.len(), MAX_READ_TEMPLATES);
        assert!(snapshot.untracked_tasks > 0);
        assert!(!serde_json::to_string(&snapshot).unwrap().contains("private-metric-canary"));
    }
}
