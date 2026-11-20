//! Explicit fixture cache observations grant no replacement reuse or cache-removal authority.
use super::*;
use crate::{
    read::NativeRead,
    test_support::{owned_case, save_cost_receipt},
};
use culprit_core::Result;
use futures_util::future::LocalBoxFuture;
use serde_json::json;
use std::{fmt::Debug, path::Path, time::Duration};

mod selection;

const DIAGNOSTIC_QUERY_CAP: u32 = 3;
const EXACT_PHASE_QUERY_CAP: u32 = 2;
const EXACT_WINDOW_QUERY_CAP: u32 = 2 * EXACT_PHASE_QUERY_CAP;
const QUIET_INTERVAL_MS: u64 = 250;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(in super::super) enum ExactCachePresence {
    Matched,
    NotFound,
}
#[derive(Debug, serde::Serialize)]
pub(in super::super) struct ExactCacheQuietWindow {
    before: ExactCachePresence,
    after: ExactCachePresence,
    quiet_interval_ms: u64,
    metadata_query_cap: u32,
    metadata_reader_retired_between_checks: bool,
    // Reader-local driver acquisition generations, never native session/login/physical-socket identities.
    before_driver_generation_bookkeeping: Option<u64>,
    after_driver_generation_bookkeeping: Option<u64>,
}

#[derive(Clone, Copy)]
enum CacheBoundary {
    Before,
    After,
}
impl CacheBoundary {
    fn receipt_name(self) -> &'static str {
        match self {
            Self::Before => "exact-cache-before-reader-retirement",
            Self::After => "exact-cache-after-reader-reopen",
        }
    }
}
#[derive(Debug, serde::Serialize)]
struct ExactCachePhase {
    presence: ExactCachePresence,
    driver_connection_generation_bookkeeping: Option<u64>,
}

#[derive(Clone, Copy)]
enum CacheSelection {
    Original,
    CurrentModule,
}
impl CacheSelection {
    fn query_cap(self) -> u32 {
        match self {
            Self::Original => EXACT_PHASE_QUERY_CAP,
            Self::CurrentModule => DIAGNOSTIC_QUERY_CAP,
        }
    }
}

impl Probe {
    /// Establish the exact original selector before source collection. This metadata preparation neither
    /// executes the application statement nor replaces its later exact pre-execution lookup.
    pub(in super::super) fn observe_original_cache_context<'a>(
        &'a self,
        original: &'a CachedStatementContext,
        directory: &'a Path,
        phase: &'static str,
    ) -> LocalBoxFuture<'a, Result<()>> {
        self.observe_cache_context(original, directory, phase, CacheSelection::Original)
    }

    /// A current module point remains diagnostic; it cannot replace the original statement incarnation.
    pub(in super::super) fn observe_current_cache_context<'a>(
        &'a self,
        original: &'a CachedStatementContext,
        directory: &'a Path,
        phase: &'static str,
    ) -> LocalBoxFuture<'a, Result<()>> {
        self.observe_cache_context(original, directory, phase, CacheSelection::CurrentModule)
    }

    fn observe_cache_context<'a>(
        &'a self,
        original: &'a CachedStatementContext,
        directory: &'a Path,
        phase: &'static str,
        selection: CacheSelection,
    ) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            owned_case::require_active(&self.cancellation)?;
            let query_cap = selection.query_cap();
            let mut budget = super::super::super::expressions::reader_budget();
            budget.max_queries = query_cap;
            let mut reader = ReadSession::new(self.pool.clone(), budget);
            let mut absence = None;
            let outcome = AssertUnwindSafe(async {
                let mut admitted = owned_case::AdmittedRead::new(&mut reader, &self.cancellation);
                let observed = match selection {
                    CacheSelection::Original => selection::observe_original(&mut admitted, self.object, original).await,
                    CacheSelection::CurrentModule => selection::observe(&mut admitted, self.object, original).await,
                };
                absence = observed.absence;
                observed.comparison
            })
            .catch_unwind()
            .await;
            let observed = settle_observation(&mut reader, outcome, directory, phase, query_cap)
                .await
                .and_then(metadata::CacheContextComparison::require_original_match);
            // The same settlement owner has closed the reader before either failure is propagated or extra receipt
            // work begins. The completed module absence remains the original error, never a fallback success.
            let diagnostic = match absence {
                Some(absence) => save_cost_receipt(
                    directory,
                    &format!("{phase}-exact-after-module-absence"),
                    &json!({
                        "phase":phase,"module_absence":absence,
                        "same_original_read_session_owner":true,
                        "metadata_queries":reader.queries(),"metadata_query_cap":query_cap,
                        "pool_shutdown_after_reader_close":format!("{:?}",self.pool.source_shutdown()),
                        "driver_connection_generation_bookkeeping":reader.connection_generation(),
                        "driver_generation_is_native_session_identity":false,
                        "original_error_preserved":observed.as_ref().err().map(|error|error.code),
                        "canonical_original_replaced":false,"cache_removal_cause_established":false,
                        "native_cost_approved":false,
                    }),
                ),
                None => Ok(()),
            };
            if observed.is_ok() {
                diagnostic?;
                owned_case::require_active(&self.cancellation)?;
            }
            observed
        })
    }

    /// The same pool owns two sequential original readers. One common window bounds both checks and
    /// the cancellable quiet interval; the before reader settles before the after reader can be admitted.
    pub(in super::super) async fn observe_exact_cache_without_collection(
        &self,
        original: &CachedStatementContext,
        directory: &Path,
    ) -> Result<ExactCacheQuietWindow> {
        owned_case::require_active(&self.cancellation)?;
        let policy = super::super::super::expressions::reader_budget();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(u64::from(policy.total_timeout_ms));
        let before = self.observe_exact_boundary(original, directory, deadline, CacheBoundary::Before).await?;
        owned_case::until_cancelled(&self.cancellation, async {
            tokio::time::sleep(Duration::from_millis(QUIET_INTERVAL_MS)).await;
            Ok(())
        })
        .await?;
        let after = self.observe_exact_boundary(original, directory, deadline, CacheBoundary::After).await?;
        owned_case::require_active(&self.cancellation)?;
        Ok(ExactCacheQuietWindow {
            before: before.presence,
            after: after.presence,
            quiet_interval_ms: QUIET_INTERVAL_MS,
            metadata_query_cap: EXACT_WINDOW_QUERY_CAP,
            metadata_reader_retired_between_checks: true,
            before_driver_generation_bookkeeping: before.driver_connection_generation_bookkeeping,
            after_driver_generation_bookkeeping: after.driver_connection_generation_bookkeeping,
        })
    }

    async fn observe_exact_boundary(
        &self,
        original: &CachedStatementContext,
        directory: &Path,
        deadline: tokio::time::Instant,
        boundary: CacheBoundary,
    ) -> Result<ExactCachePhase> {
        owned_case::require_active(&self.cancellation)?;
        let budget = exact_boundary_budget(deadline.saturating_duration_since(tokio::time::Instant::now()))?;
        let mut reader = ReadSession::new(self.pool.clone(), budget);
        let outcome = AssertUnwindSafe(async {
            let mut admitted = owned_case::AdmittedRead::new(&mut reader, &self.cancellation);
            phase(boundary.receipt_name(), &admitted);
            let presence = match boundary {
                CacheBoundary::Before => {
                    metadata::read(&mut admitted, self.object, Some(original)).await?;
                    ExactCachePresence::Matched
                }
                CacheBoundary::After => {
                    if metadata::find(&mut admitted, self.object, Some(original)).await?.is_some() {
                        ExactCachePresence::Matched
                    } else {
                        ExactCachePresence::NotFound
                    }
                }
            };
            Ok::<_, culprit_core::Error>(ExactCachePhase {
                presence,
                driver_connection_generation_bookkeeping: admitted.connection_generation(),
            })
        })
        .catch_unwind()
        .await;
        let observed =
            settle_observation(&mut reader, outcome, directory, boundary.receipt_name(), EXACT_PHASE_QUERY_CAP).await?;
        owned_case::require_active(&self.cancellation)?;
        Ok(observed)
    }
}

fn exact_boundary_budget(remaining: Duration) -> Result<CollectionBudget> {
    let mut budget = super::super::super::expressions::reader_budget();
    let remaining_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
    budget.total_timeout_ms = budget.total_timeout_ms.min(remaining_ms);
    if budget.total_timeout_ms == 0 {
        return Err(CollectionStop::TimedOut.error());
    }
    budget.max_queries = EXACT_PHASE_QUERY_CAP;
    Ok(budget)
}

async fn settle_observation<T: serde::Serialize + Debug>(
    reader: &mut ReadSession,
    outcome: std::thread::Result<Result<T>>,
    directory: &Path,
    phase: &'static str,
    query_cap: u32,
) -> Result<T> {
    // One native settlement owner for every result, cancellation and panic in either fixture observation.
    let shutdown = reader.close().await;
    let comparison = outcome.as_ref().ok().and_then(|result| result.as_ref().ok());
    eprintln!(
        "owned scalar cache observation phase={phase}; comparison={comparison:?}; queries={}; shutdown={shutdown:?}",
        reader.queries()
    );
    let receipt = save_cost_receipt(
        directory,
        phase,
        &json!({
            "phase":phase,"comparison":comparison,
            "metadata_queries":reader.queries(),"metadata_query_cap":query_cap,
            "metadata_reader_session_setup_dispatch_cap":1,
            "metadata_read_metrics":reader.read_metrics(),"shutdown":format!("{shutdown:?}"),
            "driver_connection_generation_bookkeeping":reader.connection_generation(),
            "driver_generation_is_native_session_identity":false,
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "current_context_replaces_original":false,
            "cache_transition_cause_established":false,
            "native_cost_approved":false,
        }),
    );
    let result = match outcome {
        Ok(result) => result,
        Err(original) => std::panic::resume_unwind(original),
    };
    let observed = result?;
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    receipt?;
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopened_reader_consumes_only_the_common_window_remainder_and_original_bounds() {
        let original = super::super::super::super::expressions::reader_budget();
        assert_eq!(CacheSelection::Original.query_cap(), 2);
        assert_eq!(CacheSelection::CurrentModule.query_cap(), 3);
        let small = exact_boundary_budget(Duration::from_millis(17)).unwrap();
        assert_eq!(small.total_timeout_ms, 17);
        assert_eq!(small.max_queries, 2);
        assert_eq!(small.statement_timeout_ms, original.statement_timeout_ms);
        assert_eq!(small.max_snapshot_rows, original.max_snapshot_rows);
        assert_eq!(small.page_bytes, original.page_bytes);
        assert_eq!(small.total_bytes, original.total_bytes);
        assert_eq!(
            exact_boundary_budget(Duration::from_secs(u64::from(u32::MAX))).unwrap().total_timeout_ms,
            original.total_timeout_ms
        );
        for elapsed in [Duration::ZERO, Duration::from_nanos(999_999)] {
            let error = exact_boundary_budget(elapsed).unwrap_err();
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
        }
    }
}
