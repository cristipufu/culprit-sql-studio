//! Explicit current page targets: native acquisition is identical to scans, while FrozenSource owns delivery.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions, page_targets::PageWaitGuard},
    conn::Pool,
    read::{NativeRead, ReadSession, invalid_data},
    sources,
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) struct PageWaitSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<PageWaitReader<R>>,
}
struct PageWaitReader<R: NativeRead> {
    reader: ActivityReader<R>,
    guard: Option<PageWaitGuard>,
}
impl PageWaitSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let request = bounded_request(request)?;
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::Endpoint,
        };
        Self::from_reader(ActivityReader::open(pool, options, host, port)?, &request)
    }
}
fn bounded_request(request: &CollectionRequest) -> Result<CollectionRequest> {
    request.validate()?;
    let mut bounded = request.clone();
    bounded.budget = if matches!(request.selection, CollectionSelection::PageTaskHolders { .. }) {
        crate::activity::page_targets::holders::bounded_budget(&request.budget)
    } else if matches!(request.selection, CollectionSelection::PageWaitWriters { .. }) {
        writer_budget(&request.budget)
    } else if request.focus().is_some() {
        focused_budget(&request.budget)
    } else {
        crate::page_waits::bounded_budget(&request.budget)
    };
    bounded.budget.validate()?;
    Ok(bounded)
}

/// The ordinary eighteen-query page profile plus a finite roster, guarded payload and final page/source checks.
/// This is a safety envelope, not a cost qualification or a change to default collection.
pub(super) fn writer_budget(requested: &CollectionBudget) -> CollectionBudget {
    const MAX_WRITER_QUERIES: u32 = 28;
    CollectionBudget {
        max_queries: requested.max_queries.min(MAX_WRITER_QUERIES),
        max_plan_bytes: requested.max_plan_bytes.min(64 * 1024),
        ..focused_budget(requested)
    }
}
pub(super) fn focused_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = crate::page_waits::bounded_budget(requested);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(1);
    budget.page_rows = budget.page_rows.min(1);
    budget
}
impl<R: NativeRead> PageWaitSource<R> {
    pub(super) fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.validate()?;
        if matches!(
            request.selection,
            CollectionSelection::PageWaitWriters { .. }
                | CollectionSelection::PageTaskHolders { .. }
                | CollectionSelection::PageWaitTasks { .. }
        ) && request.subject != CollectionSubject::Database
        {
            return Err(invalid_data());
        }
        if !matches!(
            &request.subject,
            CollectionSubject::Database
                | CollectionSubject::Shared {
                    resource: SharedResourceIdentity { kind: SharedResourceKind::Instance, .. }
                }
        ) {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::Unsupported,
                culprit_core::t!("collection.unsupported"),
            ));
        }
        if !matches!(
            request.selection,
            CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }
                | CollectionSelection::FocusedPageWait { .. }
                | CollectionSelection::PageWaitTasks { .. }
                | CollectionSelection::SharedPageWaitFocus { .. }
                | CollectionSelection::PageWaitWriters { .. }
                | CollectionSelection::PageTaskHolders { .. }
        ) {
            return Err(invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(PageWaitReader { reader, guard: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for PageWaitReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        if let CollectionSelection::PageTaskHolders { focus } = &request.selection {
            return self.observe_task_holders(focus).await;
        }
        if let CollectionSelection::PageWaitWriters { focus } = &request.selection {
            return self.observe_writers(focus).await;
        }
        let (captured, result) = match (&request.selection, &request.subject) {
            (CollectionSelection::FocusedPageWait { focus }, CollectionSubject::Database) => {
                let focused = self.reader.capture_focused_page_wait_targets(focus).await?;
                (focused.capture, Some(focused.result))
            }
            (CollectionSelection::SharedPageWaitFocus { focus, partition }, CollectionSubject::Shared { resource }) => {
                let focused = self.reader.capture_shared_tempdb_page_wait_focus(focus, resource, partition).await?;
                (focused.capture, Some(focused.result))
            }
            (
                CollectionSelection::PageWaitTasks { original_context, original_requests, .. },
                CollectionSubject::Database,
            ) => (self.reader.capture_paired_page_wait_targets(original_context, original_requests).await?, None),
            (
                CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
                CollectionSubject::Database,
            ) => (self.reader.capture_page_wait_targets().await?, None),
            (
                CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
                CollectionSubject::Shared { resource },
            ) => (self.reader.capture_shared_tempdb_page_wait_targets(resource).await?, None),
            _ => return Err(invalid_data()),
        };
        let database_writable = captured.database_writable();
        let task_visibility_unavailable = captured.task_visibility_unavailable();
        let partition = captured.source_partition()?;
        let target = captured.collection_target();
        let records = captured
            .snapshot
            .observations
            .into_iter()
            .map(|row| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: sources::PAGE_WAIT_TARGETS.name.into(),
                    key: row.key()?,
                    fact: OperationalFact::PageWait(Box::new(row)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut coverage =
            StateSourceCoverage::observed(sources::PAGE_WAIT_TARGETS.name.into(), records.len() as u32, false);
        coverage.stopped = if captured.limited {
            Some(CollectionStop::RowLimit)
        } else {
            task_visibility_unavailable.then_some(CollectionStop::SourceUnavailable)
        };
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::PageWaitTargets,
            observed_from: captured.snapshot.observed_from,
            observed_until: captured.snapshot.observed_until,
            records: records.len() as u32,
            // A verified frozen row population remains a bounded, non-atomic native sample. Unavailable task
            // visibility cannot become a complete empty population when coordinator rows are absent.
            sources: vec![coverage],
        };
        let observation = match result {
            Some(result) => SourceObservation::FocusedPageWait { snapshot, result: Box::new(result) },
            None => SourceObservation::PageWaitTargets {
                snapshot,
                database_writable,
                original_activity: match &request.selection {
                    CollectionSelection::PageWaitTasks { activity, .. } => Some(activity.clone()),
                    _ => None,
                },
            },
        };
        let description = CollectionDescription {
            partition,
            target,
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation,
        };
        self.guard = Some(captured.guard);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        self.reader.verify_page_wait_context(self.guard.as_ref().ok_or_else(invalid_data)?).await
    }
    fn query_count(&self) -> u32 {
        self.reader.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}

impl<R: NativeRead> PageWaitReader<R> {
    async fn observe_task_holders(&mut self, focus: &PageTaskHolderFocus) -> Result<culprit_read::FrozenObservation> {
        let captured = self.reader.capture_page_task_holders(focus).await?;
        let mut coverage = StateSourceCoverage::observed(sources::PAGE_WAIT_TARGETS.name.into(), 1, false);
        let record = TelemetryRecord::Operational(OperationalObservation {
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            key: captured.observation.key()?,
            fact: OperationalFact::PageTaskHolders(Box::new(captured.observation)),
        });
        let mut oversized = false;
        let mut source_unavailable = false;
        for field in record.definition_payloads() {
            oversized |=
                matches!(field.payload, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized });
            source_unavailable |= matches!(
                field.payload,
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::SourceUnavailable }
            );
        }
        coverage.error_code = captured.optional_sql_error;
        coverage.stopped = if captured.task_visibility_unavailable || source_unavailable {
            Some(CollectionStop::SourceUnavailable)
        } else if oversized {
            Some(CollectionStop::ByteLimit)
        } else {
            None
        };
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::PageTaskHolders,
            observed_from: captured.context.observed_from.clone(),
            observed_until: captured.context.observed_until.clone(),
            records: 1,
            sources: vec![coverage],
        };
        let records = vec![record];
        let description = CollectionDescription {
            target: captured.context.target,
            partition: captured.context.partition,
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageTaskHolders {
                snapshot,
                focus: Box::new(focus.clone()),
                database_writable: captured.context.database_writable,
            },
        };
        self.guard = Some(captured.guard);
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn observe_writers(&mut self, focus: &PageWaitWriterFocus) -> Result<culprit_read::FrozenObservation> {
        let captured = self.reader.capture_page_wait_writers(focus).await?;
        let mut coverage = StateSourceCoverage::observed(sources::PAGE_WAIT_TARGETS.name.into(), 1, false);
        if captured.observation.candidates.iter().any(|candidate| {
            matches!(candidate.plan, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized })
                || candidate.request.as_ref().is_some_and(|request| {
                    matches!(
                        request.statement,
                        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
                    )
                })
        }) {
            coverage.stopped = Some(CollectionStop::ByteLimit);
        }
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::PageWaitWriters,
            observed_from: captured.context.observed_from.clone(),
            observed_until: captured.context.observed_until.clone(),
            records: 1,
            sources: vec![coverage],
        };
        let records = vec![TelemetryRecord::Operational(OperationalObservation {
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            key: captured.observation.key()?,
            fact: OperationalFact::PageWaitWriters(Box::new(captured.observation)),
        })];
        let description = CollectionDescription {
            target: captured.context.target,
            partition: captured.context.partition,
            source: sources::PAGE_WAIT_TARGETS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitWriters {
                snapshot,
                focus: Box::new(focus.clone()),
                database_writable: captured.context.database_writable,
            },
        };
        self.guard = Some(captured.guard);
        Ok(culprit_read::FrozenObservation { description, records })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for PageWaitSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.inner.describe().await
    }
    fn query_count(&self) -> u32 {
        self.inner.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.inner.read_page(stream, cursor).await
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.inner.finish().await
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests;
