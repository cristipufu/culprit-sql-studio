//! Witnessed statement-statistics absence permits one existing direct plan/SQL-binding check, never a fallback.
use super::{ExactCachePresence, metadata};
use crate::{read::NativeRead, relational_inputs::Bindings};
use culprit_core::{Error, ErrorCode, ErrorDetail, Result, telemetry::*};
use serde::Serialize;

pub(super) struct ObservedSelection {
    pub comparison: Result<metadata::CacheContextComparison>,
    pub absence: Option<ModuleAbsence>,
}

#[derive(Debug, Serialize)]
pub(super) struct ModuleAbsence {
    original_stop: CollectionStop,
    module_query_ordinal: u32,
    exact_queries_after: u32,
    exact_original: std::result::Result<ExactCachePresence, NativeFailure>,
    direct_binding_after_exact_absence: Option<DirectBindingObservation>,
}

#[derive(Debug, Serialize)]
struct DirectBindingObservation {
    queries_before: u32,
    queries_after: u32,
    matching_plan_sql_binding_visible: std::result::Result<bool, NativeFailure>,
}

#[derive(Debug, Serialize)]
struct NativeFailure {
    code: ErrorCode,
    stop: Option<CollectionStop>,
}
impl From<Error> for NativeFailure {
    fn from(error: Error) -> Self {
        Self {
            code: error.code,
            stop: match error.detail {
                Some(ErrorDetail::CollectionStopped(stop)) => Some(stop),
                _ => None,
            },
        }
    }
}

/// This exact prerequisite uses the same selected native reader and verification as record_original.
/// A missing or changed original ends preparation, without a current-module or direct-binding fallback.
pub(super) async fn observe_original(
    reader: &mut impl NativeRead,
    object: i64,
    original: &CachedStatementContext,
) -> ObservedSelection {
    // The real metadata future owns native acquisition state; keep it off this selection's inline frame.
    let comparison = Box::pin(metadata::read(reader, object, Some(original)))
        .await
        .map(|selected| metadata::CacheContextComparison::between(original, &selected.context));
    ObservedSelection { comparison, absence: None }
}

pub(super) async fn observe(
    reader: &mut impl NativeRead,
    object: i64,
    original: &CachedStatementContext,
) -> ObservedSelection {
    match metadata::find(reader, object, None).await {
        Ok(Some(current)) => ObservedSelection {
            comparison: Ok(metadata::CacheContextComparison::between(original, &current.context)),
            absence: None,
        },
        Ok(None) => {
            let module_query_ordinal = reader.queries();
            let exact_original = metadata::find(reader, object, Some(original))
                .await
                .map(
                    |selected| {
                        if selected.is_some() { ExactCachePresence::Matched } else { ExactCachePresence::NotFound }
                    },
                )
                .map_err(NativeFailure::from);
            let exact_queries_after = reader.queries();
            let direct_binding_after_exact_absence = if matches!(&exact_original, Ok(ExactCachePresence::NotFound)) {
                Some(observe_direct_binding(reader, original).await)
            } else {
                None
            };
            ObservedSelection {
                // The original module-only assertion still fails, even when the diagnostic exact selector matches.
                comparison: Err(CollectionStop::SourceUnavailable.error()),
                absence: Some(ModuleAbsence {
                    original_stop: CollectionStop::SourceUnavailable,
                    module_query_ordinal,
                    exact_queries_after,
                    exact_original,
                    direct_binding_after_exact_absence,
                }),
            }
        }
        Err(error) => ObservedSelection { comparison: Err(error), absence: None },
    }
}

async fn observe_direct_binding(
    reader: &mut impl NativeRead,
    original: &CachedStatementContext,
) -> DirectBindingObservation {
    let queries_before = reader.queries();
    let result = async {
        let mut bindings = Bindings::default();
        if !bindings.admit(original)? {
            return Err(crate::read::invalid_data());
        }
        let verified = bindings.verify(reader).await?;
        // This is only the native handle-to-SQL binding. It cannot reestablish a vanished statement-statistics
        // generation, its creation time, current cache residency, an eviction mechanism, or the original result.
        Ok(verified.contains(original))
    }
    .await;
    DirectBindingObservation {
        queries_before,
        queries_after: reader.queries(),
        matching_plan_sql_binding_visible: result.map_err(NativeFailure::from),
    }
}

#[cfg(test)]
mod tests;
