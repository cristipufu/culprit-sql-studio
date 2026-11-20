//! A bounded fixture observer checks original cache selection between real memory-acquisition queries.
//! Source and observer retain independent native reader ownership, metrics and admission budgets.
use super::*;
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    read::{NativeRead, Parameter, ReadError, ReadResult},
    test_support::{owned_case, save_cost_receipt},
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use serde::Serialize;
use serde_json::json;
use std::path::Path;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum MemoryPhase {
    ObservationContextLayoutAndGrants,
    SqlEnrichment,
    FinalContext,
}
struct PhaseSpan {
    phase: MemoryPhase,
    first_exclusive: u32,
    last_inclusive: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TraceState {
    Observing,
    BaselineFailed,
    SourceFailed,
    ObserverFailed,
    OriginalNotFound,
    CallerCancelled,
}
#[derive(Debug, Serialize)]
struct QueryObservation {
    phase: Option<MemoryPhase>,
    source_ordinal: u32,
    template_id: String,
    redacted_template: Option<DefinitionPayload>,
    source_query_completed: bool,
    native_rows: Option<usize>,
    original_found_after: Option<bool>,
    error_code: Option<ErrorCode>,
    observer_queries_after: u32,
}
#[derive(Debug, Serialize)]
struct PostCloseObservation {
    source_queries: u32,
    observer_queries_before: u32,
    observer_queries_after: u32,
    original_found: Option<bool>,
    error_code: Option<ErrorCode>,
}

struct PhaseRead<'a, S: NativeRead, O: NativeRead> {
    source: &'a mut S,
    observer: &'a mut O,
    original: &'a CachedStatementContext,
    object: i64,
    cancellation: &'a CollectionCancellation,
    baseline_matched: bool,
    state: TraceState,
    first_error: Option<Error>,
    observations: Vec<QueryObservation>,
    post_close: Option<PostCloseObservation>,
    diagnostic_stop_injected: bool,
    source_shutdown: Option<SourceShutdown>,
    observer_shutdown: Option<SourceShutdown>,
    closed: Option<SourceShutdown>,
}
impl<'a, S: NativeRead, O: NativeRead> PhaseRead<'a, S, O> {
    fn new(
        source: &'a mut S,
        observer: &'a mut O,
        original: &'a CachedStatementContext,
        object: i64,
        cancellation: &'a CollectionCancellation,
    ) -> Self {
        Self {
            source,
            observer,
            original,
            object,
            cancellation,
            baseline_matched: false,
            state: TraceState::Observing,
            first_error: None,
            observations: vec![],
            post_close: None,
            diagnostic_stop_injected: false,
            source_shutdown: None,
            observer_shutdown: None,
            closed: None,
        }
    }
    async fn baseline(&mut self) -> Result<()> {
        let mut observer = owned_case::AdmittedRead::new(&mut *self.observer, self.cancellation);
        metadata::read(&mut observer, self.object, Some(self.original)).await?;
        self.baseline_matched = true;
        Ok(())
    }
    async fn find_original(&mut self) -> Result<bool> {
        let mut observer = owned_case::AdmittedRead::new(&mut *self.observer, self.cancellation);
        Ok(metadata::find(&mut observer, self.object, Some(self.original)).await?.is_some())
    }
    async fn close_source(&mut self) -> SourceShutdown {
        if let Some(shutdown) = self.source_shutdown {
            return shutdown;
        }
        let shutdown = self.source.close().await;
        self.source_shutdown = Some(shutdown);
        shutdown
    }
    async fn observe_after_source_close(&mut self) -> Result<()> {
        if self.post_close.is_some() || self.closed.is_some() {
            return Err(crate::read::invalid_data());
        }
        if self.close_source().await != SourceShutdown::Quiescent {
            return Err(CollectionStop::CancellationUnconfirmed.error());
        }
        if self.state != TraceState::Observing {
            return Ok(());
        }
        let before = self.observer.queries();
        let result = self.find_original().await;
        self.post_close = Some(PostCloseObservation {
            source_queries: self.source.queries(),
            observer_queries_before: before,
            observer_queries_after: self.observer.queries(),
            original_found: result.as_ref().ok().copied(),
            error_code: result.as_ref().err().map(|error| error.code),
        });
        match result {
            Ok(false) => self.state = TraceState::OriginalNotFound,
            Err(error) => {
                self.state = TraceState::ObserverFailed;
                self.first_error = Some(error.clone());
                return Err(error);
            }
            Ok(true) => {}
        }
        Ok(())
    }
    fn assign_phases(&mut self, spans: &[PhaseSpan]) {
        for point in &mut self.observations {
            point.phase = spans
                .iter()
                .find(|span| point.source_ordinal > span.first_exclusive && point.source_ordinal <= span.last_inclusive)
                .map(|span| span.phase);
        }
    }
}
#[async_trait::async_trait]
impl<S: NativeRead, O: NativeRead> NativeRead for PhaseRead<'_, S, O> {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        if self.state != TraceState::Observing || self.source_shutdown.is_some() || self.closed.is_some() {
            return Err(ReadError::Stopped(CollectionStop::Cancelled));
        }
        if let Err(error) = owned_case::require_active(self.cancellation) {
            self.state = TraceState::CallerCancelled;
            self.first_error = Some(error.clone());
            return Err(ReadError::Source(error));
        }
        if !self.baseline_matched {
            if let Err(error) = self.baseline().await {
                self.state = TraceState::BaselineFailed;
                self.first_error = Some(error.clone());
                return Err(ReadError::Source(error));
            }
        }
        if let Err(error) = owned_case::require_active(self.cancellation) {
            self.state = TraceState::CallerCancelled;
            self.first_error = Some(error.clone());
            return Err(ReadError::Source(error));
        }
        let before = self.source.queries();
        let result = self.source.query(sql, parameters, limit).await;
        let ordinal = self.source.queries();
        let template_id = crate::statement_origin::template_id(sql);
        let redacted_template = self
            .source
            .read_metrics()
            .and_then(|metrics| metrics.templates.into_iter().find(|template| template.template_id == template_id))
            .map(|template| template.statement);
        let mut point = QueryObservation {
            phase: None,
            source_ordinal: ordinal,
            template_id,
            redacted_template,
            source_query_completed: result.is_ok(),
            native_rows: result.as_ref().ok().map(Vec::len),
            original_found_after: None,
            error_code: None,
            observer_queries_after: self.observer.queries(),
        };
        let rows = match result {
            Ok(rows) => rows,
            Err(original) => {
                self.state = TraceState::SourceFailed;
                self.first_error = Some(match &original {
                    ReadError::Source(error) => error.clone(),
                    ReadError::Stopped(stop) => stop.error(),
                });
                point.error_code = Some(match &original {
                    ReadError::Source(error) => error.code,
                    ReadError::Stopped(stop) => stop.error().code,
                });
                if ordinal > before {
                    self.observations.push(point);
                }
                return Err(original);
            }
        };
        let checked = self.find_original().await;
        point.observer_queries_after = self.observer.queries();
        match checked {
            Ok(found) => {
                point.original_found_after = Some(found);
                self.observations.push(point);
                if !found {
                    self.state = TraceState::OriginalNotFound;
                    self.diagnostic_stop_injected = true;
                    // The original source query already completed. This test-owner stop prevents the next
                    // dispatch; it is not native Attention or evidence that the source query was cancelled.
                    return Err(ReadError::Stopped(CollectionStop::Cancelled));
                }
                Ok(rows)
            }
            Err(error) => {
                self.state = TraceState::ObserverFailed;
                self.first_error = Some(error.clone());
                point.error_code = Some(error.code);
                self.observations.push(point);
                Err(ReadError::Source(error))
            }
        }
    }
    fn queries(&self) -> u32 {
        self.source.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.source.connection_generation()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.source.read_metrics()
    }
    fn observe_session_work(&mut self, observation: crate::read::session_work::Observation) {
        self.source.observe_session_work(observation);
    }
    async fn close(&mut self) -> SourceShutdown {
        if let Some(shutdown) = self.closed {
            return shutdown;
        }
        let source = self.close_source().await;
        let observer = self.observer.close().await;
        self.observer_shutdown = Some(observer);
        let shutdown = if source == SourceShutdown::Quiescent && observer == SourceShutdown::Quiescent {
            SourceShutdown::Quiescent
        } else {
            SourceShutdown::Unconfirmed
        };
        self.closed = Some(shutdown);
        shutdown
    }
}

impl Probe {
    pub(in super::super) fn observe_memory_source_phases<'a>(
        &'a self,
        original: &'a CachedStatementContext,
        observer_pool: Arc<Pool>,
        directory: &'a Path,
    ) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            owned_case::require_active(&self.cancellation)?;
            let source_budget = crate::request_memory::bounded_budget(&CollectionBudget::default());
            let observer_cap = source_budget
                .max_queries
                .checked_add(2)
                .and_then(|checks| checks.checked_mul(2))
                .ok_or_else(crate::read::invalid_data)?;
            let mut observer_budget = super::super::super::expressions::reader_budget();
            observer_budget.max_queries = observer_cap;
            let mut source = ReadSession::new(self.pool.clone(), source_budget.clone());
            let mut observer = ReadSession::new(observer_pool, observer_budget.clone());
            let mut trace = PhaseRead::new(&mut source, &mut observer, original, self.object, &self.cancellation);
            let options = ReadOptions { budget: source_budget.clone(), identity: IdentityDetail::Endpoint };
            let mut reader = ActivityReader::new(
                owned_case::AdmittedRead::new(&mut trace, &self.cancellation),
                options,
                "127.0.0.1".into(),
                self.port,
            );
            let mut spans = Vec::new();
            let outcome = AssertUnwindSafe(async {
                let first = reader.query_count();
                let observed = reader.observe_memory().await;
                spans.push(PhaseSpan {
                    phase: MemoryPhase::ObservationContextLayoutAndGrants,
                    first_exclusive: first,
                    last_inclusive: reader.query_count(),
                });
                let mut frame = observed?;
                let first = reader.query_count();
                let enriched = reader.enrich_memory(&mut frame).await;
                spans.push(PhaseSpan {
                    phase: MemoryPhase::SqlEnrichment,
                    first_exclusive: first,
                    last_inclusive: reader.query_count(),
                });
                enriched?;
                let first = reader.query_count();
                let verified = reader.verify_memory_context(&frame.owner).await;
                spans.push(PhaseSpan {
                    phase: MemoryPhase::FinalContext,
                    first_exclusive: first,
                    last_inclusive: reader.query_count(),
                });
                verified?;
                Ok::<_, Error>(())
            })
            .catch_unwind()
            .await;
            let acquisition_completed = outcome.as_ref().is_ok_and(|result| result.is_ok());
            // ActivityReader only borrows the decorator. The same source owner closes before the retained
            // observer's final check; no source query can be admitted after that close.
            drop(reader);
            let post_close = if acquisition_completed {
                AssertUnwindSafe(trace.observe_after_source_close()).catch_unwind().await
            } else {
                Ok(Ok(()))
            };
            let shutdown = trace.close().await;
            trace.assign_phases(&spans);
            let receipt = save_cost_receipt(
                directory,
                "memory-source-phase-trace",
                &json!({
                    "diagnostic_only":true,"baseline_exact_original_matched":trace.baseline_matched,
                    "original_cached_statement":original,"original_module_object_id":self.object,
                    "native_acquisition_completed":acquisition_completed,
                    "state":trace.state,"phases":trace.observations,
                    "after_source_close":trace.post_close,
                    "post_close_error_code":post_close.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
                    "first_error_code":trace.first_error.as_ref().map(|error|error.code),
                    "source_queries":trace.source.queries(),"source_query_cap":source_budget.max_queries,
                    "source_budget":source_budget,"observer_budget":observer_budget,
                    "acquisition_error_code":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
                    "panicked":outcome.is_err() || post_close.is_err(),
                    "diagnostic_stop_injected_after_native_completion":trace.diagnostic_stop_injected,
                    "observer_queries":trace.observer.queries(),"observer_query_cap":observer_cap,
                    "source_read_metrics":trace.source.read_metrics(),"observer_read_metrics":trace.observer.read_metrics(),
                    "source_shutdown":trace.source_shutdown.map(|value|format!("{value:?}")),
                    "observer_shutdown":trace.observer_shutdown.map(|value|format!("{value:?}")),
                    "combined_shutdown":format!("{shutdown:?}"),
                    "source_driver_generation_bookkeeping":trace.source.connection_generation(),
                    "observer_driver_generation_bookkeeping":trace.observer.connection_generation(),
                    "driver_generations_are_native_identities":false,
                    "source_session_setup_dispatch_cap":1,"observer_session_setup_dispatch_cap":1,
                    "scalar_preparation_route_prime_and_metadata_dispatch_cap":16,
                    "existing_grant_fixture_dispatch_cap_separate":21,
                    "native_fixture_admission_transport_dispatch_cap_separate":3,
                    "total_declared_dispatch_cap":73,
                    "observer_work_excluded_from_source_counters":true,
                    "native_active_query_cancellation_exercised":false,
                    "canonical_capture_performed":false,
                    "native_source_factory_acceptance_established":false,
                    "grant_acceptance_established":false,"cache_removal_cause_established":false,"native_cost_approved":false,
                }),
            );
            let result = match outcome {
                Ok(result) => result,
                Err(original) => std::panic::resume_unwind(original),
            };
            let post_close_result = match post_close {
                Ok(result) => result,
                Err(original) => std::panic::resume_unwind(original),
            };
            // Only our classified observer stop may complete this diagnostic. A source/observer error remains
            // its original failure, and no classification can waive either original reader's settlement debt.
            if let Some(error) = trace.first_error.as_ref() {
                return Err(error.clone());
            }
            if trace.state != TraceState::OriginalNotFound {
                result?;
            }
            post_close_result?;
            if shutdown != SourceShutdown::Quiescent {
                return Err(CollectionStop::CancellationUnconfirmed.error());
            }
            receipt?;
            owned_case::require_active(&self.cancellation)
        })
    }
}
#[cfg(test)]
mod tests;
