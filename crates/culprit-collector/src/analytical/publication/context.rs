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
    FocusedBlockingContext {
        snapshot: &'a ActivitySnapshotDescription,
        focus_key: String,
        binding: FocusedRequestBinding,
        membership: FocusedRequestMembership,
        request_end: BlockingRequestState,
        closure: &'a BlockingClosureDescription,
    },
    Snapshot(&'a StateSnapshotDescription),
    SourceReadContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: &'a str,
        execution_key: String,
    },
    MysqlEvents {
        snapshot: &'a StateSnapshotDescription,
        metadata: &'a MysqlEventsMetadata,
    },
    DatabasePerformanceCountersContext {
        snapshot: &'a StateSnapshotDescription,
        database_writable: bool,
    },
    BackupHistory {
        snapshot: &'a StateSnapshotDescription,
        scope: &'a MssqlBackupHistoryScope,
        database_writable: bool,
    },
    MssqlAgentJobs {
        snapshot: &'a StateSnapshotDescription,
        metadata: &'a MssqlAgentJobsMetadata,
    },
    Jobs {
        snapshot: &'a StateSnapshotDescription,
        metadata: &'a TimescaleJobsMetadata,
    },
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
    CurrentRequestWorkContext {
        profile: CurrentRequestWorkProfile,
        #[serde(flatten)]
        context: FocusedRequestContext<'a>,
    },
    ColumnstoreRowgroupsContext {
        snapshot: &'a StateSnapshotDescription,
        target_key: String,
        index_type: u8,
        database_writable: bool,
    },
    CacheEntryMemoryContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
    },
    PageWaitWritersContext {
        snapshot: &'a StateSnapshotDescription,
        focus_key: String,
        database_writable: Option<bool>,
    },
    PageWaitTargetsContext {
        snapshot: &'a StateSnapshotDescription,
        database_writable: Option<bool>,
        original_activity_key: Option<String>,
    },
    PageTaskHoldersContext {
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
        SourceObservation::FocusedBlocking { snapshot, result } => PublicObservation::FocusedBlockingContext {
            snapshot,
            focus_key: result.focus.key()?,
            binding: result.binding,
            membership: result.membership,
            request_end: result.request_end,
            closure: &result.closure,
        },
        SourceObservation::Snapshot(value) => PublicObservation::Snapshot(value),
        SourceObservation::SourceRead { snapshot, focus_key, execution } => PublicObservation::SourceReadContext {
            snapshot,
            focus_key,
            execution_key: NativeSourceReadScope::from_context(execution)?.key()?,
        },
        SourceObservation::PageWaitTargets { snapshot, database_writable, original_activity } => {
            PublicObservation::PageWaitTargetsContext {
                snapshot,
                database_writable: *database_writable,
                original_activity_key: original_activity.as_ref().map(DiagnosticCaptureRef::key).transpose()?,
            }
        }
        SourceObservation::Jobs { snapshot, metadata } => PublicObservation::Jobs { snapshot, metadata },
        SourceObservation::MysqlEvents { snapshot, metadata } => PublicObservation::MysqlEvents { snapshot, metadata },
        SourceObservation::DatabasePerformanceCounters { snapshot, database_writable, .. } => {
            PublicObservation::DatabasePerformanceCountersContext { snapshot, database_writable: *database_writable }
        }
        SourceObservation::BackupHistory { snapshot, scope, database_writable } => {
            PublicObservation::BackupHistory { snapshot, scope, database_writable: *database_writable }
        }
        SourceObservation::MssqlAgentJobs { snapshot, metadata } => {
            PublicObservation::MssqlAgentJobs { snapshot, metadata }
        }
        SourceObservation::ColumnstoreRowgroups { snapshot, target, index_type, database_writable } => {
            PublicObservation::ColumnstoreRowgroupsContext {
                snapshot,
                target_key: target.key()?,
                index_type: *index_type,
                database_writable: *database_writable,
            }
        }
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
        SourceObservation::PageTaskHolders { snapshot, focus, database_writable } => {
            PublicObservation::PageTaskHoldersContext {
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
        SourceObservation::CurrentRequestWork { profile, snapshot, result } => {
            PublicObservation::CurrentRequestWorkContext {
                profile: *profile,
                context: FocusedRequestContext::new(snapshot, result)?,
            }
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
        SourceObservation::PagedSnapshot(_) | SourceObservation::PagedSourceRead { .. } => {
            return Err(DeliveryProblem::Integrity.error());
        }
    };
    serde_json::to_value(projected).map_err(|_| DeliveryProblem::Integrity.error())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod database_counter_tests {
    use super::*;
    #[test]
    fn selected_database_public_observation_retains_scope_counts_and_role_without_recovery_owned_proof() {
        let (source, _) = crate::analytical::test_support::database_performance_counter_evidence(
            crate::analytical::test_support::DatabasePerformanceCounterCase::Exact,
        );
        let public = observation(&source.observation).unwrap();
        assert_eq!(public["kind"], "database-performance-counters-context");
        assert_eq!(public["data"]["database_writable"], true);
        assert_eq!(public["data"]["snapshot"]["records"], 1);
        let text = serde_json::to_string(&public).unwrap();
        for private in [
            "native_database",
            "private-database-guid-canary",
            "private-fork-guid-canary",
            "private-file-guid-canary",
            "private-process-canary",
        ] {
            assert!(!text.contains(private));
        }
    }
}
