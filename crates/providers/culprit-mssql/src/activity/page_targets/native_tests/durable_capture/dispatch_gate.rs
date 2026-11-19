//! The original native task reader is prepared while the owned INSERT gate still holds. Its next existing
//! query waits for that gate's retained owner; no observer, replacement query or synthetic wait is introduced.
use super::{Captured, Fixture, OwnedCaptures, invalid};
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    conn::Pool,
    read::{NativeRead, Parameter, ReadError, ReadResult, ReadSession, session_work::Observation},
    value::Rec,
};
use culprit_collector::capture::{CollectionCancellation, SourceFactory};
use culprit_core::{Result, telemetry::*};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

const DISPATCH_PREPARATION_TIMEOUT: Duration = Duration::from_secs(3);

struct DispatchPause {
    ready: oneshot::Sender<()>,
    released: oneshot::Receiver<bool>,
}
struct GateControl {
    ready: oneshot::Receiver<()>,
    released: oneshot::Sender<bool>,
}
fn dispatch_gate() -> (DispatchPause, GateControl) {
    let (prepared, ready) = oneshot::channel();
    let (released, allowed) = oneshot::channel();
    (DispatchPause { ready: prepared, released: allowed }, GateControl { ready, released })
}

struct GateFactory {
    pool: Arc<Pool>,
    port: u16,
    pause: Mutex<Option<DispatchPause>>,
}
#[async_trait::async_trait]
impl SourceFactory for GateFactory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        let pause = self
            .pause
            .lock()
            .map_err(|_| invalid("owned task dispatch gate was poisoned"))?
            .take()
            .ok_or_else(|| invalid("owned task source was opened more than once"))?;
        let CollectionSelection::PageWaitTasks { original_requests, .. } = &request.selection else {
            return Err(invalid("owned task dispatch gate received another collection"));
        };
        let options = ReadOptions { budget: request.budget.clone(), identity: IdentityDetail::Endpoint };
        let reader = GatedRead {
            inner: ReadSession::new(self.pool.clone(), request.budget.clone()),
            pause: Some(pause),
            preparation: Preparation::Pending,
            original_requests: original_requests.clone(),
        };
        crate::telemetry::page_wait_source_from_reader(
            ActivityReader::new(reader, options, "127.0.0.1".into(), self.port),
            request,
        )
    }
}

enum Preparation {
    Pending,
    Observed,
    Unavailable,
}
struct GatedRead<R> {
    inner: R,
    pause: Option<DispatchPause>,
    preparation: Preparation,
    original_requests: Vec<ActivityRequestSelector>,
}
#[async_trait::async_trait]
impl<R: NativeRead> NativeRead for GatedRead<R> {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        if matches!(self.preparation, Preparation::Unavailable) && self.pause.take().is_some() {
            return Err(invalid("owned task reader had no typed native preparation point").into());
        }
        let population_dispatch = matches!(self.preparation, Preparation::Observed) && self.pause.is_some();
        if population_dispatch && let Some(pause) = self.pause.take() {
            pause.ready.send(()).map_err(|_| ReadError::Stopped(CollectionStop::Cancelled))?;
            if !pause.released.await.unwrap_or(false) {
                return Err(ReadError::Stopped(CollectionStop::Cancelled));
            }
        }
        let result = self.inner.query(sql, parameters, row_limit).await;
        if population_dispatch && let Ok(rows) = &result {
            eprintln!(
                "{}",
                serde_json::json!({
                    "phase":"owned-native-task-population-before-original-filter",
                    "population":PopulationReceipt::from_rows(rows, &self.original_requests),
                })
            );
        }
        result
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.inner.connection_generation()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    fn observe_session_work(&mut self, observation: Observation) {
        if self.pause.is_some() {
            match &observation {
                Observation::Point(_) => self.preparation = Preparation::Observed,
                Observation::Unavailable(_) => self.preparation = Preparation::Unavailable,
                Observation::Started => {}
            }
        }
        self.inner.observe_session_work(observation);
    }
    async fn close(&mut self) -> SourceShutdown {
        self.pause.take();
        self.inner.close().await
    }
}

/// Read-only counts from the same already-returned native population. No statement/session tokens or SQL
/// are printed, and the production decoder still owns filtering, validation and every qualification verdict.
#[derive(Default, serde::Serialize)]
struct PopulationReceipt {
    returned_rows: usize,
    context_only_rows: usize,
    decoded_requests: usize,
    native_task_rows: usize,
    original_request_keys: usize,
    matching_statement_tokens: usize,
    different_statement_tokens: usize,
    missing_statement_tokens: usize,
    other_request_generations: usize,
    decode_failures: usize,
}
impl PopulationReceipt {
    fn from_rows(rows: &[Rec], original: &[ActivityRequestSelector]) -> Self {
        let mut receipt = Self { returned_rows: rows.len(), ..Self::default() };
        for row in rows {
            let record = match crate::activity::rows::record(row) {
                Ok(Some(record)) => record,
                Ok(None) => {
                    receipt.context_only_rows += 1;
                    continue;
                }
                Err(_) => {
                    receipt.decode_failures += 1;
                    continue;
                }
            };
            receipt.decoded_requests += 1;
            match crate::read::optional_text(row, "task_address") {
                Ok(Some(_)) => receipt.native_task_rows += 1,
                Ok(None) => {}
                Err(_) => receipt.decode_failures += 1,
            }
            let Some(original) = original.iter().find(|original| original.key == record.facts.key) else {
                receipt.other_request_generations += 1;
                continue;
            };
            receipt.original_request_keys += 1;
            match &record.facts.statement_identity {
                Some(current) if *current == original.statement_identity => receipt.matching_statement_tokens += 1,
                Some(_) => receipt.different_statement_tokens += 1,
                None => receipt.missing_statement_tokens += 1,
            }
        }
        receipt
    }
}

/// The collector and gate owner are joined. Even failed preparation releases the retained seed gate, and a
/// failed release cancels the same collector before its paused native query can be admitted.
pub(crate) async fn capture_at_gate_release(
    captures: &mut OwnedCaptures,
    fixture: &mut Fixture,
    selection: CollectionSelection,
) -> Result<(Captured, Instant)> {
    let (pause, control) = dispatch_gate();
    let factory = GateFactory {
        pool: fixture.pool.as_ref().ok_or_else(|| invalid("owned task pool absent"))?.clone(),
        port: fixture.port,
        pause: Mutex::new(Some(pause)),
    };
    let cancellation = CollectionCancellation::default();
    let capture = captures.capture_with_factory(selection, &cancellation, &factory);
    let gate = release_when_ready(control, &cancellation, fixture.release_holder_gate());
    let (captured, released) = tokio::join!(capture, gate);
    let captured = captured?;
    Ok((captured, released?))
}

async fn release_when_ready(
    control: GateControl,
    cancellation: &CollectionCancellation,
    release: impl std::future::Future<Output = Result<()>>,
) -> Result<Instant> {
    let prepared = match tokio::time::timeout(DISPATCH_PREPARATION_TIMEOUT, control.ready).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(invalid("owned task reader ended before its dispatch gate")),
        Err(_) => Err(CollectionStop::TimedOut.error()),
    };
    if prepared.is_err() {
        cancellation.cancel();
    }
    let released = release.await;
    let at = Instant::now();
    let allowed = prepared.is_ok() && released.is_ok();
    if !allowed {
        cancellation.cancel();
    }
    let _ = control.released.send(allowed);
    prepared?;
    released?;
    Ok(at)
}

#[cfg(test)]
mod tests;
