//! Focused compiler admission uses the shared frozen-source owner and the existing native request/gateway readers.
use crate::{
    activity::{ActivityReader, IdentityDetail, ReadOptions},
    conn::Pool,
    read::{NativeRead, ReadSession, invalid_data},
};
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

pub(super) fn focused_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        page_rows: MAX_FOCUSED_COMPILER_GATEWAYS as u32 + 1,
        max_snapshot_rows: MAX_FOCUSED_COMPILER_GATEWAYS as u32 + 1,
        max_pages: 4,
        page_bytes: 256 * 1024,
        total_bytes: 1024 * 1024,
        max_sql_bytes: 16 * 1024,
        max_queries: 12 + ReadSession::SESSION_SETUP_QUERIES,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
        ..CollectionBudget::default()
    })
}

pub(super) struct CompilerAdmissionSource<R: NativeRead = ReadSession> {
    inner: culprit_read::FrozenSource<CompilerObservationReader<R>>,
}
struct CompilerObservationReader<R: NativeRead> {
    reader: ActivityReader<R>,
    owner: Option<crate::local_source::Context>,
}
impl CompilerAdmissionSource {
    pub fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let options = ReadOptions { budget: focused_budget(&request.budget), identity: IdentityDetail::Endpoint };
        Self::from_reader(ActivityReader::open_collection(pool, options, host, port)?, request)
    }
}
impl<R: NativeRead> CompilerAdmissionSource<R> {
    pub(super) fn from_reader(reader: ActivityReader<R>, request: &CollectionRequest) -> Result<Self> {
        request.validate()?;
        if !matches!(
            request.selection,
            CollectionSelection::FocusedCompilationAdmission { .. }
                | CollectionSelection::SharedCompilationAdmissionFocus { .. }
        ) {
            return Err(invalid_data());
        }
        Ok(Self { inner: culprit_read::FrozenSource::new(CompilerObservationReader { reader, owner: None }, request)? })
    }
}
#[async_trait::async_trait]
impl<R: NativeRead> culprit_read::ObservationReader for CompilerObservationReader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let focus = request.focus().ok_or_else(invalid_data)?;
        let shared = match &request.subject {
            CollectionSubject::Database => None,
            CollectionSubject::Shared { resource } => {
                Some((resource, request.selection.shared_partition().ok_or_else(invalid_data)?))
            }
        };
        let captured = self.reader.capture_focused_compiler_admission(focus, shared).await?;
        let count = captured.observations.len() as u32;
        let mut coverage = StateSourceCoverage::observed(
            crate::sources::COMPILATION_GATEWAYS.name.into(),
            count,
            captured.result.membership == CompilationAdmissionMembership::Observed
                && captured.gateway_failure.is_none(),
        );
        if let Some(error) = captured.gateway_failure {
            coverage.error_code = Some(error.code);
            coverage.stopped = Some(CollectionStop::from_error(&error));
        }
        let description = CollectionDescription {
            target: captured.context.target.clone(),
            partition: captured.context.partition.clone(),
            source: crate::sources::COMPILATION_GATEWAYS.name.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedCompilationAdmission {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::CompilerAdmission,
                    observed_from: captured.context.observed_from,
                    observed_until: captured.context.observed_until,
                    records: count,
                    sources: vec![coverage],
                },
                result: Box::new(captured.result),
            },
        };
        self.owner = Some(captured.owner);
        Ok(culprit_read::FrozenObservation {
            description,
            records: captured.observations.into_iter().map(TelemetryRecord::Operational).collect(),
        })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        self.reader.verify_compiler_admission(self.owner.as_ref().ok_or_else(invalid_data)?).await
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
#[async_trait::async_trait]
impl<R: NativeRead> TelemetrySource for CompilerAdmissionSource<R> {
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
