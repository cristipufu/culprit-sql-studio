//! A witnessed exact absence keeps its original failure. A conditional current point is diagnostic only.
use super::*;
use crate::{read::NativeRead, test_support::save_cost_receipt};
use culprit_core::{ErrorCode, ErrorDetail, Result, telemetry::*};
use serde::Serialize;
use serde_json::json;
use std::path::Path;

pub(super) struct CheckedMetadata {
    pub metadata: Result<metadata::StatementMetadata>,
    pub transition: Option<CacheTransition>,
}

#[derive(Debug, Serialize)]
pub(super) struct CacheTransition {
    original_stop: CollectionStop,
    exact_query_ordinal: u32,
    current_module_queries_after: u32,
    module_followup_dispatch_cap: u32,
    current_module: CurrentModule,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum CurrentModule {
    Observed { comparison: metadata::CacheContextComparison },
    PredicatesObserved { point: Box<metadata::NativePredicatePoint> },
    NotFound,
    Unavailable { code: ErrorCode, stop: Option<CollectionStop> },
}

pub(super) fn save_completed(directory: &Path, plans: &[String]) -> Result<()> {
    let plans = plans.iter().map(|raw| DefinitionPayload::plan(raw, culprit_core::Engine::Mssql)).collect::<Vec<_>>();
    save_cost_receipt(
        directory,
        "completed-execution-before-post-metadata",
        &json!({
            "original_response_completely_drained":true,
            "redacted_native_plans":plans,
            "post_execution_cache_binding_established":false,
            "runtime_input_tuple_proof_retained":false,
            "current_context_replaces_original":false,
            "cache_transition_cause_established":false,
            "native_cost_approved":false,
        }),
    )
}

const MODULE_FOLLOWUP_DISPATCH_CAP: u32 = 2;

enum Followup {
    Module,
    OriginalPredicates { reader_query_cap: u32 },
}

pub(super) fn read<'a, R: NativeRead>(
    reader: &'a mut R,
    object: i64,
    original: &'a CachedStatementContext,
) -> LocalBoxFuture<'a, Box<CheckedMetadata>> {
    read_with_followup(reader, object, original, Followup::Module)
}

pub(super) fn read_before_execution<'a, R: NativeRead>(
    reader: &'a mut R,
    object: i64,
    original: &'a CachedStatementContext,
    reader_query_cap: u32,
) -> LocalBoxFuture<'a, Box<CheckedMetadata>> {
    read_with_followup(reader, object, original, Followup::OriginalPredicates { reader_query_cap })
}

fn read_with_followup<'a, R: NativeRead>(
    reader: &'a mut R,
    object: i64,
    original: &'a CachedStatementContext,
    followup: Followup,
) -> LocalBoxFuture<'a, Box<CheckedMetadata>> {
    Box::pin(async move {
        let selected = metadata::find(reader, object, Some(original)).await;
        match selected {
            Ok(Some(selected)) => Box::new(CheckedMetadata { metadata: Ok(selected), transition: None }),
            Ok(None) => {
                let exact_query_ordinal = reader.queries();
                let current_module = match followup {
                    Followup::Module => match metadata::find(reader, object, None).await {
                        Ok(Some(current)) => CurrentModule::Observed {
                            comparison: metadata::CacheContextComparison::between(original, &current.context),
                        },
                        Ok(None) => CurrentModule::NotFound,
                        Err(error) => CurrentModule::unavailable(error),
                    },
                    Followup::OriginalPredicates { reader_query_cap } => {
                        // The same exclusive reader borrow reserves both possible follow-up dispatches before
                        // either starts. Its existing per-query/deadline/byte/cancellation admission still applies.
                        if !reader
                            .queries()
                            .checked_add(MODULE_FOLLOWUP_DISPATCH_CAP)
                            .is_some_and(|end| end <= reader_query_cap)
                        {
                            CurrentModule::unavailable(CollectionStop::QueryLimit.error())
                        } else {
                            match metadata::compare_predicates(reader, object, original).await {
                                Ok(Some(point)) => CurrentModule::PredicatesObserved { point },
                                Ok(None) => CurrentModule::NotFound,
                                Err(error) => CurrentModule::unavailable(error),
                            }
                        }
                    }
                };
                Box::new(CheckedMetadata {
                    metadata: Err(CollectionStop::SourceUnavailable.error()),
                    transition: Some(CacheTransition {
                        original_stop: CollectionStop::SourceUnavailable,
                        exact_query_ordinal,
                        current_module_queries_after: reader.queries(),
                        module_followup_dispatch_cap: MODULE_FOLLOWUP_DISPATCH_CAP,
                        current_module,
                    }),
                })
            }
            Err(error) => Box::new(CheckedMetadata { metadata: Err(error), transition: None }),
        }
    })
}

impl CurrentModule {
    fn unavailable(error: culprit_core::Error) -> Self {
        Self::Unavailable {
            code: error.code,
            stop: match error.detail {
                Some(ErrorDetail::CollectionStopped(stop)) => Some(stop),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests;
