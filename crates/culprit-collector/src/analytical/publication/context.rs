//! Public coverage is an explicit projection, not a partially serialized private native source description.
use culprit_core::{ObservedWaitResource, Result, telemetry::*};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
enum PublicObservation<'a> {
    QueryHistory(&'a QueryHistoryDescription),
    RetainedSamples(&'a SampleHistoryDescription),
    RetainedEvents(&'a EventHistoryDescription),
    Activity(&'a ActivitySnapshotDescription),
    Snapshot(&'a StateSnapshotDescription),
    FocusedPageWaitContext {
        snapshot: &'a StateSnapshotDescription,
        result: Box<FocusResult<'a>>,
    },
    FocusedRequestMemoryContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
        source: FocusSourceState,
        membership: RequestMemoryMembership,
    },
    FocusedCompilationAdmissionContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
        source: FocusSourceState,
        membership: CompilationAdmissionMembership,
    },
    MaintenanceProgressContext(FocusedRequestContext<'a>),
    CurrentRequestWorkContext(FocusedRequestContext<'a>),
    CacheEntryMemoryContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
    },
    PageWaitWritersContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
        database_writable: Option<bool>,
    },
    DiagnosticContext(Value),
}
#[derive(Serialize)]
struct FocusedRequestContext<'a> {
    snapshot: &'a StateSnapshotDescription,
    focus_key: Option<String>,
    database_writable: Option<bool>,
    binding: FocusedRequestBinding,
    membership: FocusedRequestMembership,
}
impl<'a> FocusedRequestContext<'a> {
    fn new(snapshot: &'a StateSnapshotDescription, result: &FocusedRequestResult) -> Result<Self> {
        Ok(Self {
            snapshot,
            focus_key: result.focus.as_ref().map(|focus| focus.key()).transpose()?,
            database_writable: result.database_writable,
            binding: result.binding,
            membership: result.membership,
        })
    }
}

#[derive(Serialize)]
struct FocusResult<'a> {
    focus: Focus<'a>,
    observed_from: &'a str,
    observed_until: &'a str,
    source: FocusSourceState,
    request: PageWaitRequestState,
    wait: PageWaitRelation,
}
#[derive(Serialize)]
struct Focus<'a> {
    key: String,
    parent: &'a DiagnosticEvidenceRef,
    source: &'a str,
    epoch: &'a str,
    original_observed_at: &'a str,
    statement_identity: &'a NativeIdentity,
    resource: &'a ObservedWaitResource,
}

pub(super) fn observation(value: &SourceObservation) -> Result<Value> {
    let projected = match value {
        SourceObservation::QueryHistory(value) => PublicObservation::QueryHistory(value),
        SourceObservation::RetainedSamples(value) => PublicObservation::RetainedSamples(value),
        SourceObservation::RetainedEvents(value) => PublicObservation::RetainedEvents(value),
        SourceObservation::Activity(value) => PublicObservation::Activity(value),
        SourceObservation::Snapshot(value) => PublicObservation::Snapshot(value),
        SourceObservation::CacheEntryMemory { snapshot, focus } => {
            PublicObservation::CacheEntryMemoryContext { snapshot, focus_key: focus.key()? }
        }
        SourceObservation::PageWaitWriters { snapshot, focus, database_writable } => {
            PublicObservation::PageWaitWritersContext {
                snapshot,
                focus_key: focus.key()?,
                database_writable: *database_writable,
            }
        }
        SourceObservation::FocusedRequestMemory { snapshot, result } => {
            PublicObservation::FocusedRequestMemoryContext {
                snapshot,
                focus_key: result.focus.key()?,
                source: result.source,
                membership: result.membership,
            }
        }
        SourceObservation::FocusedCompilationAdmission { snapshot, result } => {
            PublicObservation::FocusedCompilationAdmissionContext {
                snapshot,
                focus_key: result.focus.key()?,
                source: result.source,
                membership: result.membership,
            }
        }
        SourceObservation::MaintenanceProgress { snapshot, result } => {
            PublicObservation::MaintenanceProgressContext(FocusedRequestContext::new(snapshot, result)?)
        }
        SourceObservation::CurrentRequestWork { snapshot, result } => {
            PublicObservation::CurrentRequestWorkContext(FocusedRequestContext::new(snapshot, result)?)
        }
        SourceObservation::FocusedPageWait { snapshot, result } => PublicObservation::FocusedPageWaitContext {
            snapshot,
            result: Box::new(FocusResult {
                focus: Focus {
                    key: result.focus.key()?,
                    parent: &result.focus.participant().parent,
                    source: &result.focus.participant().source,
                    epoch: &result.focus.participant().epoch,
                    original_observed_at: &result.focus.participant().original_observed_at,
                    statement_identity: &result.focus.participant().statement_identity,
                    resource: &result.focus.resource,
                },
                observed_from: &result.observed_from,
                observed_until: &result.observed_until,
                source: result.source,
                request: result.request,
                wait: result.wait,
            }),
        },
        SourceObservation::Diagnostics(value) => {
            PublicObservation::DiagnosticContext(super::super::investigations::diagnostic_context(value)?)
        }
        SourceObservation::PagedSnapshot(_) => return Err(DeliveryProblem::Integrity.error()),
    };
    serde_json::to_value(projected).map_err(|_| DeliveryProblem::Integrity.error())
}

#[cfg(test)]
mod tests;
