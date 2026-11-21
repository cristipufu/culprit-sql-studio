//! Observer handles record existing owner transitions; they dispatch and authorize no native work.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

mod operation;
mod reader;
pub use operation::{ProviderFactoryObservation, ProviderOperationObservation};
pub use reader::{ProviderReaderObservation, ProviderReaderObserver};

pub struct ProviderReadObservation {
    context: ProviderReadContext,
}
#[derive(Clone, Default)]
pub struct ProviderReadContext(Option<Arc<Mutex<State>>>);
struct State {
    id: String,
    kind: ProviderReadKind,
    started_at: String,
    started: Instant,
    frozen: Option<ProviderAcquisitionSummary>,
    route: Option<SourceAdmissionRoute>,
    engine: Option<Engine>,
    admitted: bool,
    native_started: bool,
    settlement: Option<(ProviderReadDisposition, Result<()>)>,
    complete_coverage: bool,
    operation_claimed: bool,
    gaps: Vec<ProviderReadGap>,
    factory: Option<ProviderFactorySpan>,
    readers: BTreeMap<u32, ProviderReaderSummary>,
    next_reader: u32,
    omitted_readers: u32,
    closed: bool,
}
impl State {
    fn gap(&mut self, gap: ProviderReadGap) {
        if !self.gaps.contains(&gap) {
            self.gaps.push(gap);
        }
    }
}
impl ProviderReadObservation {
    /// The service allocates this exact invocation ID before authentication; it is not a capture revision.
    pub fn begin(id: String, kind: ProviderReadKind) -> Result<Self> {
        if !valid_id(&id) {
            return Err(invalid());
        }
        Ok(Self {
            context: ProviderReadContext(Some(Arc::new(Mutex::new(State {
                id,
                kind,
                started_at: now(),
                started: Instant::now(),
                frozen: None,
                route: None,
                engine: None,
                admitted: false,
                native_started: false,
                settlement: None,
                complete_coverage: false,
                operation_claimed: false,
                gaps: vec![],
                factory: None,
                readers: BTreeMap::new(),
                next_reader: 1,
                omitted_readers: 0,
                closed: false,
            })))),
        })
    }
    pub fn operation(&self) -> Result<ProviderOperationObservation> {
        let state = self.context.0.as_ref().expect("an observation owns its context");
        let mut state = locked(state);
        if state.operation_claimed || state.closed {
            return Err(invalid());
        }
        state.operation_claimed = true;
        Ok(ProviderOperationObservation { context: self.context.clone() })
    }
    pub fn context(&self) -> ProviderReadContext {
        self.context.clone()
    }
    pub fn finish<T>(self, original: Result<T>) -> ProviderAttempt<T> {
        self.finish_original(Some(original))
    }
    fn finish_original<T>(self, original: Option<Result<T>>) -> ProviderAttempt<T> {
        let failure =
            original.as_ref().and_then(|original| original.as_ref().err()).map(ProviderReadFailure::from_error);
        let state = self.context.0.as_ref().expect("an observation owns its context");
        let mut state = locked(state);
        let acquisition = freeze(&mut state, failure);
        let finalization = state.settlement.as_ref().map(|(_, result)| result.clone()).unwrap_or_else(|| {
            if acquisition.native_disposition == ProviderReadDisposition::Unconfirmed {
                Err(CollectionStop::CancellationUnconfirmed.error())
            } else {
                Ok(())
            }
        });
        ProviderAttempt { original, finalization, acquisition }
    }
    pub fn finish_call<T>(self, result: Result<ProviderCall<T>>) -> ProviderAttempt<T> {
        match result {
            Err(error) => self.finish(Err(error)),
            Ok(call) => {
                let (original, finalization, acquisition) = call.into_parts();
                let bound = self.context.0.as_ref().is_some_and(|state| {
                    let state = locked(state);
                    acquisition.as_ref().zip(state.frozen.as_ref()).is_some_and(|(actual, expected)| {
                        let output_is_valid = original.is_some()
                            || (finalization.is_err()
                                && actual.original_failure.is_none()
                                && actual.finalization_failure.is_some()
                                && state.settlement.as_ref().is_some_and(|(_, result)| result.is_err()));
                        actual == expected && actual.observation_id == state.id && output_is_valid
                    })
                });
                let mut attempt = self.finish_original(original);
                if !bound {
                    attempt.finalization = Err(invalid());
                    attempt.acquisition.finalization_failure =
                        attempt.finalization.as_ref().err().map(ProviderReadFailure::from_error);
                    attempt.acquisition.native_disposition = ProviderReadDisposition::Unconfirmed;
                    attempt.acquisition.coverage =
                        ProviderReadCoverage::Unavailable { reason: ProviderReadGap::InvalidObservation };
                    attempt.acquisition.readers.clear();
                    return attempt;
                }
                if attempt.finalization.is_ok() {
                    attempt.acquisition.finalization_failure =
                        finalization.as_ref().err().map(ProviderReadFailure::from_error);
                    attempt.finalization = finalization;
                }
                attempt
            }
        }
    }
}
fn freeze(state: &mut State, failure: Option<ProviderReadFailure>) -> ProviderAcquisitionSummary {
    if let Some(frozen) = &state.frozen {
        return frozen.clone();
    }
    state.closed = true;
    let (disposition, finalization) = state.settlement.clone().unwrap_or_else(|| {
        if state.admitted || state.native_started {
            (ProviderReadDisposition::Unconfirmed, Err(CollectionStop::CancellationUnconfirmed.error()))
        } else {
            (ProviderReadDisposition::NotStarted, Ok(()))
        }
    });
    let mut gaps = state.gaps.clone();
    for reader in state.readers.values() {
        if reader.queries.is_none() || reader.work.is_none() {
            push_gap(&mut gaps, ProviderReadGap::MissingReaderSnapshot);
        }
        if !reader.complete_setup() {
            push_gap(&mut gaps, ProviderReadGap::MissingSetupSnapshot);
        }
        if reader.disposition != ProviderReaderDisposition::Quiescent
            || reader.work.as_deref().is_some_and(|work| !settled_work(work))
            || reader.setup_work.as_deref().is_some_and(|work| !settled_work(work))
        {
            push_gap(&mut gaps, ProviderReadGap::ReaderAbandoned);
        }
        if [reader.work.as_deref(), reader.setup_work.as_deref()]
            .into_iter()
            .flatten()
            .any(|work| work.untracked_tasks > 0)
        {
            push_gap(&mut gaps, ProviderReadGap::UntrackedTasks);
        }
    }
    if state.omitted_readers > 0 {
        push_gap(&mut gaps, ProviderReadGap::OmittedReaders);
    }
    let coverage = if !state.complete_coverage {
        if !state.readers.is_empty() {
            push_gap(&mut gaps, ProviderReadGap::PartialReaderCoverage);
        }
        if gaps.is_empty() {
            ProviderReadCoverage::Unavailable { reason: ProviderReadGap::NotInstrumented }
        } else {
            ProviderReadCoverage::Partial { gaps }
        }
    } else if gaps.is_empty() {
        ProviderReadCoverage::Complete
    } else {
        ProviderReadCoverage::Partial { gaps }
    };
    let mut summary = ProviderAcquisitionSummary {
        observation_id: state.id.clone(),
        kind: state.kind,
        route: state.route.clone(),
        engine: state.engine,
        client_started_at: state.started_at.clone(),
        client_finished_at: now(),
        client_elapsed_ms: state.started.elapsed().as_secs_f64() * 1000.0,
        native_disposition: disposition,
        coverage,
        factory: state.factory.clone().map(Box::new),
        original_failure: failure,
        finalization_failure: finalization.as_ref().err().map(ProviderReadFailure::from_error),
        readers: state.readers.values().cloned().collect(),
        omitted_readers: state.omitted_readers,
    };
    if !summary.has_ordered_client_clocks() {
        add_gap(&mut summary, ProviderReadGap::ClientClockChanged);
    }
    if crate::serialization::json_size(&summary, MAX_PROVIDER_ACQUISITION_BYTES).is_err() {
        summary.omitted_readers = summary.omitted_readers.saturating_add(summary.readers.len() as u32);
        summary.readers.clear();
        summary.coverage = ProviderReadCoverage::Unavailable { reason: ProviderReadGap::Oversized };
    }
    state.frozen = Some(summary.clone());
    summary
}

impl ProviderReadContext {
    pub fn unobserved() -> Self {
        Self::default()
    }
    pub fn is_observed(&self) -> bool {
        self.0.is_some()
    }
    fn update(&self, work: impl FnOnce(&mut State)) {
        if let Some(state) = &self.0 {
            let mut state = locked(state);
            if !state.closed {
                work(&mut state);
            }
        }
    }
    pub fn complete_reader_coverage(&self) {
        self.update(|state| state.complete_coverage = true);
    }
    pub fn partial_reader_coverage(&self, gap: ProviderReadGap) {
        self.update(|state| state.gap(gap));
    }
    pub fn reader(&self, engine: Engine) -> ProviderReaderObservation {
        let mut id = None;
        self.update(|state| {
            if state.readers.len() >= MAX_PROVIDER_READERS {
                state.omitted_readers = state.omitted_readers.saturating_add(1);
                state.gap(ProviderReadGap::OmittedReaders);
                return;
            }
            let next = state.next_reader;
            state.next_reader += 1;
            id = Some(next);
            state.readers.insert(
                next,
                ProviderReaderSummary {
                    reader_id: next,
                    engine,
                    setup_accounting: ProviderSetupAccounting::Unknown,
                    queries: None,
                    setup_queries: None,
                    work: None,
                    setup_work: None,
                    disposition: ProviderReaderDisposition::Pending,
                },
            );
        });
        ProviderReaderObservation { context: self.clone(), id, finished: false }
    }
}
fn push_gap(gaps: &mut Vec<ProviderReadGap>, gap: ProviderReadGap) {
    if !gaps.contains(&gap) {
        gaps.push(gap);
    }
}
fn add_gap(summary: &mut ProviderAcquisitionSummary, gap: ProviderReadGap) {
    match &mut summary.coverage {
        ProviderReadCoverage::Complete => summary.coverage = ProviderReadCoverage::Partial { gaps: vec![gap] },
        ProviderReadCoverage::Partial { gaps } => push_gap(gaps, gap),
        ProviderReadCoverage::Unavailable { .. } => (),
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn locked(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    match state.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.gap(ProviderReadGap::InvalidObservation);
            state.settlement =
                Some((ProviderReadDisposition::Unconfirmed, Err(CollectionStop::CancellationUnconfirmed.error())));
            state
        }
    }
}
