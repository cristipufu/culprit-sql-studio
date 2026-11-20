//! Native failure diagnostics opt into the same bounded job reader used by explicit retained collection.
use crate::{
    connect::Connector,
    native,
    read::{NativeRead, ReadSession},
    sources,
};
use culprit_core::{Coverage, Result, Snapshot, message, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};

pub(crate) async fn collect(connector: &Connector, snapshot: &mut Snapshot) {
    let budget = native::jobs::bounded_budget(&CollectionBudget::default());
    let mut reader = ReadSession::new(connector.clone(), budget.clone());
    let (captured, coverage) = collect_from(&mut reader, &connector.host, connector.port, &budget).await;
    snapshot.operations.jobs = captured
        .map(|snapshot| Box::new(JobsSnapshot::Timescale { snapshot: Box::new(snapshot), read_complete: true }));
    coverage.append_to(snapshot);
}
async fn collect_from<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    budget: &CollectionBudget,
) -> (Option<TimescaleJobsSnapshot>, Coverage) {
    let result = native::jobs::capture(reader, host, port, budget).await;
    let shutdown = reader.close().await;
    let mut coverage = Coverage::default();
    let captured = match result {
        Ok(captured) if shutdown == SourceShutdown::Quiescent => {
            for (source, state) in [
                (&sources::JOBS, &captured.sources[0]),
                (&sources::JOB_RUNS, &captured.sources[1]),
                (&sources::CONTINUOUS_AGGREGATES, &captured.sources[2]),
            ] {
                if matches!(state.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty) {
                    coverage.recorded(source, state.state, None);
                } else {
                    coverage.recorded_message(source, state.state, message!("provider.postgres.jobs.coverage"));
                }
            }
            Some(captured)
        }
        Ok(_) => None,
        Err(error) => {
            coverage.source_failed(&sources::JOBS, &error);
            coverage.source_failed(&sources::JOB_RUNS, &error);
            coverage.source_failed(&sources::CONTINUOUS_AGGREGATES, &error);
            None
        }
    };
    if shutdown == SourceShutdown::Unconfirmed {
        let error = CollectionStop::CancellationUnconfirmed.error();
        coverage.source_failed(&sources::JOBS, &error);
        coverage.source_failed(&sources::JOB_RUNS, &error);
        coverage.source_failed(&sources::CONTINUOUS_AGGREGATES, &error);
    }
    (captured, coverage)
}

struct Reader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}
pub(crate) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    if request.selection != (CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs }) {
        return Err(native::invalid());
    }
    let mut request = request.clone();
    request.budget = native::jobs::bounded_budget(&request.budget);
    let reader = Reader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        verified: None,
    };
    Ok(Box::new(FrozenSource::new(reader, &request)?))
}
#[async_trait::async_trait]
impl<R: NativeRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let captured = native::jobs::capture(&mut self.io, &self.host, self.port, &request.budget).await?;
        let description = CollectionDescription {
            target: captured.context.target,
            partition: captured.context.partition,
            source: TIMESCALE_JOBS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Jobs {
                metadata: Box::new(captured.metadata),
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::Jobs,
                    observed_from: captured.context.observed_from,
                    observed_until: captured.context.observed_until,
                    records: captured.observations.len() as u32,
                    sources: captured.sources,
                },
            },
        };
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
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
#[cfg(test)]
mod tests;
