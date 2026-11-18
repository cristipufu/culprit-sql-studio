//! One bounded native Agent reader serves explicit scan diagnostics and the retained Snapshot owner.
mod decode;
mod layout;
mod native;
use native::{ReadSubject, capture};
mod sql;
use crate::{
    conn::Pool,
    read::{
        NativeRead, Parameter, ReadError, ReadSession, integer, invalid_data, optional_integer, optional_text,
        stops_remaining_reads, text,
    },
    value::Rec,
};
use culprit_core::{Coverage, Error, ErrorCode, Result, Snapshot, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

const MAX_JOB_ROWS: u32 = 128;
const MAX_STEP_ROWS: u32 = 256;
const MAX_HISTORY_ROWS: u32 = 256;
const MAX_SOURCE_QUERIES: u32 = 13;

pub(crate) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        page_rows: 64,
        max_snapshot_rows: MAX_MSSQL_AGENT_JOB_OBSERVATIONS as u32,
        max_queries: MAX_SOURCE_QUERIES,
        max_pages: 32,
        page_bytes: 256 * 1024,
        total_bytes: 2 * 1024 * 1024,
        statement_timeout_ms: 1000,
        total_timeout_ms: 5000,
        ..Default::default()
    })
}
pub(crate) fn source(
    pool: Arc<Pool>,
    request: &CollectionRequest,
    host: String,
    port: u16,
) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs }) {
        return Err(invalid_data());
    }
    if !match &request.subject {
        CollectionSubject::Database => true,
        CollectionSubject::Shared { resource } => resource.kind == SharedResourceKind::Instance,
    } {
        return Err(unsupported());
    }
    let mut bounded = request.clone();
    bounded.budget = bounded_budget(&request.budget);
    let reader = Reader { io: ReadSession::new(pool, bounded.budget.clone()), host, port, verified: None };
    Ok(Box::new(FrozenSource::new(reader, &bounded)?))
}
struct Reader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}
#[async_trait::async_trait]
impl<R: NativeRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let captured =
            capture(&mut self.io, ReadSubject::Selected(&request.subject), &request.budget, &self.host, self.port)
                .await?;
        let description = description(&captured);
        if !description.matches_request(request) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.verified = Some(description.clone());
        Ok(FrozenObservation {
            description,
            records: captured.observations.into_iter().map(TelemetryRecord::Operational).collect(),
        })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(description))
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
fn description(read: &MssqlAgentJobsSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: read.context.target.clone(),
        partition: read.context.partition.clone(),
        source: MSSQL_AGENT_JOBS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlAgentJobs {
            metadata: Box::new(read.metadata.clone()),
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Jobs,
                observed_from: read.context.observed_from.clone(),
                observed_until: read.context.observed_until.clone(),
                records: read.observations.len() as u32,
                sources: read.sources.clone(),
            },
        },
    }
}

pub(crate) async fn collect(pool: Arc<Pool>, host: &str, port: u16, snapshot: &mut Snapshot) {
    let budget = bounded_budget(&CollectionBudget::default());
    let mut io = ReadSession::new(pool, budget.clone());
    let (read, coverage) = collect_from(&mut io, &budget, host, port).await;
    snapshot.operations.jobs =
        read.map(|snapshot| Box::new(JobsSnapshot::MssqlAgent { snapshot: Box::new(snapshot), read_complete: true }));
    coverage.append_to(snapshot);
}
async fn collect_from<R: NativeRead>(
    io: &mut R,
    budget: &CollectionBudget,
    host: &str,
    port: u16,
) -> (Option<MssqlAgentJobsSnapshot>, Coverage) {
    let result = capture(io, ReadSubject::Connected, budget, host, port).await;
    let shutdown = io.close().await;
    let mut coverage = Coverage::default();
    let read = match result {
        Ok(read) if shutdown == SourceShutdown::Quiescent => match source_coverage(&read) {
            Ok(recorded) => {
                coverage = recorded;
                Some(read)
            }
            Err(error) => {
                for source in SOURCES {
                    coverage.source_failed(source, &error);
                }
                None
            }
        },
        Ok(_) => None,
        Err(error) => {
            for source in SOURCES {
                coverage.source_failed(source, &error);
            }
            None
        }
    };
    if shutdown == SourceShutdown::Unconfirmed {
        for source in SOURCES {
            coverage.source_failed(source, &CollectionStop::CancellationUnconfirmed.error());
        }
    }
    (read, coverage)
}
const SOURCES: [&culprit_core::Source; 4] = [
    &crate::sources::AGENT_JOBS,
    &crate::sources::AGENT_STEPS,
    &crate::sources::AGENT_HISTORY,
    &crate::sources::AGENT_CONTEXT,
];
fn source_coverage(read: &MssqlAgentJobsSnapshot) -> Result<Coverage> {
    let mapped = read
        .sources
        .iter()
        .map(|outcome| {
            SOURCES
                .into_iter()
                .find(|source| source.name == outcome.source)
                .map(|source| (source, outcome))
                .ok_or_else(invalid_data)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut coverage = Coverage::default();
    for (source, outcome) in mapped {
        match outcome.state {
            culprit_core::SourceState::Collected
            | culprit_core::SourceState::Empty
            | culprit_core::SourceState::NotApplicable => coverage.recorded(source, outcome.state, None),
            _ => coverage.recorded_message(
                source,
                outcome.state,
                culprit_core::message!("view.mssql_agent_jobs.source.gap"),
            ),
        }
    }
    Ok(coverage)
}

fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("view.mssql_agent_jobs.unavailable"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
