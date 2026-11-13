//! Process health and expected work, independent of a healthy source or destination. Metrics contain no SQL.
mod http;
#[cfg(test)]
mod tests;
use culprit_core::{Result, telemetry::*};
pub use http::HealthServer;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_METRICS_BYTES: usize = 32 * 1024 * 1024;
const MAX_STATUS_AGE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct CollectorHealth {
    state: Arc<Mutex<State>>,
}
#[derive(Clone)]
struct State {
    started: Instant,
    initialized: bool,
    schedules: BTreeMap<(String, String), Schedule>,
    delivery_errors: BTreeMap<String, u64>,
    deliveries: u64,
    outbox: Option<OutboxHealth>,
    backlog: Option<DeliveryBacklog>,
    archive_backlog: Option<DeliveryBacklog>,
    source_fenced: bool,
    source_enabled: bool,
}
#[derive(Clone)]
struct Schedule {
    id: String,
    connection: String,
    maximum_silence: Duration,
    last_attempt: Option<Instant>,
    last_complete: Option<Instant>,
    last_failed: bool,
    attempts: u64,
    complete: u64,
    incomplete: u64,
}
impl Schedule {
    fn record(&mut self, status: &CollectionStatus, now: Instant) {
        self.last_attempt = Some(now);
        self.attempts = self.attempts.saturating_add(1);
        match status {
            CollectionStatus::Complete => {
                self.complete = self.complete.saturating_add(1);
                self.last_complete = Some(now);
                self.last_failed = false;
            }
            CollectionStatus::Incomplete(stopped) => {
                self.incomplete = self.incomplete.saturating_add(1);
                if *stopped != CollectionStop::SourceNotReady {
                    self.last_failed = true;
                }
            }
        }
    }
    fn overdue(&self, now: Instant, started: Instant) -> bool {
        now.duration_since(self.last_complete.unwrap_or(started)) > self.maximum_silence
    }
}
#[derive(Clone)]
struct OutboxHealth {
    bytes: u64,
    limit: u64,
    captures: u32,
    source_debts: u32,
    unconfirmed_sources: u32,
    updated_at: Instant,
    unfinished: u32,
}
pub struct HealthReply {
    pub ready: bool,
    pub metrics: Vec<u8>,
}

impl CollectorHealth {
    pub fn new(inventory: &FleetInventory, shard: &str) -> Result<Self> {
        inventory.validate(shard)?;
        let mut schedules = BTreeMap::new();
        for target in inventory.targets.iter().filter(|t| t.shard == shard) {
            for schedule in &target.schedules {
                let key = (target.id.clone(), schedule.id.clone());
                let id = digest(&serde_json::to_vec(&key).map_err(|_| DeliveryProblem::Integrity.error())?);
                let milliseconds = 2 * u64::from(schedule.every_seconds) * 1000
                    + u64::from(schedule.jitter_seconds) * 1000
                    + u64::from(schedule.budget.total_timeout_ms);
                schedules.insert(
                    key,
                    Schedule {
                        id,
                        connection: target.connection.clone(),
                        maximum_silence: Duration::from_millis(milliseconds),
                        last_attempt: None,
                        last_complete: None,
                        last_failed: false,
                        attempts: 0,
                        complete: 0,
                        incomplete: 0,
                    },
                );
            }
        }
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                started: Instant::now(),
                initialized: false,
                schedules,
                delivery_errors: BTreeMap::new(),
                deliveries: 0,
                outbox: None,
                backlog: None,
                archive_backlog: None,
                source_fenced: false,
                source_enabled: true,
            })),
        })
    }
    pub fn initialized(&self) -> Result<()> {
        self.with(|state| state.initialized = true)
    }
    pub fn source_paused(&self) -> Result<()> {
        self.with(|state| state.source_fenced = true)
    }
    pub fn source_disabled(&self) -> Result<()> {
        self.with(|state| state.source_enabled = false)
    }
    pub fn collection(&self, event: &FleetCollectionEvent) -> Result<()> {
        self.with(|state| {
            let Some(schedule) = state.schedules.get_mut(&(event.target.clone(), event.schedule.clone())) else {
                return;
            };
            if schedule.connection != event.connection {
                return;
            }
            schedule.record(&event.status, Instant::now());
        })
    }
    pub fn delivery(&self, result: &Result<DeliveryEntry>) -> Result<()> {
        self.with(|state| match result {
            Ok(_) => state.deliveries = state.deliveries.saturating_add(1),
            Err(error) => {
                let name = match error.detail {
                    Some(culprit_core::ErrorDetail::Delivery(problem)) => serde_json::to_value(problem)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".into()),
                    _ => "unknown".into(),
                };
                let count = state.delivery_errors.entry(name).or_default();
                *count = count.saturating_add(1);
            }
        })
    }
    pub fn outbox(&self, status: &OutboxStatus) -> Result<()> {
        self.with(|state| {
            state.outbox = Some(OutboxHealth {
                bytes: status.charged_bytes,
                limit: status.limits.max_bytes,
                captures: status.captures,
                source_debts: status.source_debts,
                unconfirmed_sources: status.unconfirmed_sources,
                updated_at: Instant::now(),
                unfinished: status.unfinished,
            })
        })
    }
    pub fn backlog(&self, backlog: DeliveryBacklog) -> Result<()> {
        self.with(|state| state.backlog = Some(backlog))
    }
    pub fn archive_backlog(&self, backlog: DeliveryBacklog) -> Result<()> {
        self.with(|state| state.archive_backlog = Some(backlog))
    }
    pub fn reply(&self) -> Result<HealthReply> {
        use std::io::Write;
        let state = self.state.lock().map_err(|_| DeliveryProblem::Storage.error())?.clone();
        let now = Instant::now();
        let mut body = crate::durable_fs::LimitedBuffer::new(MAX_METRICS_BYTES);
        let ready = state.ready(now);
        writeln!(body, "culprit_collector_uptime_seconds {}", now.duration_since(state.started).as_secs())
            .map_err(metric_error)?;
        for schedule in state.schedules.values() {
            let overdue = state.source_enabled && schedule.overdue(now, state.started);
            for (name, count) in [
                ("attempts_total", schedule.attempts),
                ("completed_operations_total", schedule.complete),
                ("incomplete_operations_total", schedule.incomplete),
                ("overdue", u64::from(overdue)),
            ] {
                writeln!(body, "culprit_collector_schedule_{name}{{schedule=\"{}\"}} {count}", schedule.id)
                    .map_err(metric_error)?;
            }
            // An absent timestamp remains unknown, rather than an invented successful age of zero.
            for (name, at) in [
                ("last_attempt_age_seconds", schedule.last_attempt),
                ("last_completed_operation_age_seconds", schedule.last_complete),
            ] {
                if let Some(at) = at {
                    writeln!(
                        body,
                        "culprit_collector_schedule_{name}{{schedule=\"{}\"}} {}",
                        schedule.id,
                        now.duration_since(at).as_secs()
                    )
                    .map_err(metric_error)?;
                }
            }
        }
        for (problem, count) in &state.delivery_errors {
            writeln!(body, "culprit_collector_delivery_errors_total{{problem=\"{problem}\"}} {count}")
                .map_err(metric_error)?;
        }
        writeln!(body, "culprit_collector_delivery_steps_total {}", state.deliveries).map_err(metric_error)?;
        writeln!(body, "culprit_collector_source_fenced {}", u8::from(state.source_fenced)).map_err(metric_error)?;
        writeln!(body, "culprit_collector_source_enabled {}", u8::from(state.source_enabled)).map_err(metric_error)?;
        for (owner, backlog) in [("archive", &state.archive_backlog), ("delivery", &state.backlog)] {
            let Some(backlog) = backlog else { continue };
            writeln!(body, "culprit_collector_{owner}_pending_ranges {}", backlog.pending).map_err(metric_error)?;
            writeln!(body, "culprit_collector_{owner}_quarantined_ranges {}", backlog.quarantined)
                .map_err(metric_error)?;
            if let Some(oldest) = backlog.oldest_pending_collection_ms {
                let elapsed = chrono::Utc::now().timestamp_millis().saturating_sub(oldest);
                let age = elapsed.max(0) / 1000;
                writeln!(body, "culprit_collector_{owner}_oldest_pending_clock_ahead {}", u8::from(elapsed < 0))
                    .map_err(metric_error)?;
                writeln!(body, "culprit_collector_{owner}_oldest_pending_age_seconds {age}").map_err(metric_error)?;
            }
        }
        if let Some(outbox) = &state.outbox {
            for (name, count) in [
                ("charged_bytes", outbox.bytes),
                ("limit_bytes", outbox.limit),
                ("captures", u64::from(outbox.captures)),
                ("source_debts", u64::from(outbox.source_debts)),
                ("unconfirmed_sources", u64::from(outbox.unconfirmed_sources)),
                ("unfinished", u64::from(outbox.unfinished)),
            ] {
                writeln!(body, "culprit_collector_outbox_{name} {count}").map_err(metric_error)?;
            }
        }
        writeln!(body, "culprit_collector_ready {}", u8::from(ready)).map_err(metric_error)?;
        Ok(HealthReply { ready, metrics: body.into_bytes() })
    }
    fn with(&self, update: impl FnOnce(&mut State)) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| DeliveryProblem::Storage.error())?;
        update(&mut state);
        Ok(())
    }
    pub fn ready(&self) -> Result<bool> {
        Ok(self.state.lock().map_err(|_| DeliveryProblem::Storage.error())?.ready(Instant::now()))
    }
}
impl State {
    fn ready(&self, now: Instant) -> bool {
        self.initialized
            && !self.source_fenced
            && self.outbox.as_ref().is_some_and(|outbox| {
                outbox.unconfirmed_sources == 0 && now.duration_since(outbox.updated_at) <= MAX_STATUS_AGE
            })
            && (!self.source_enabled
                || self
                    .schedules
                    .values()
                    .all(|schedule| !schedule.last_failed && !schedule.overdue(now, self.started)))
    }
}
fn metric_error(_: std::io::Error) -> culprit_core::Error {
    DeliveryProblem::Capacity.error()
}
