//! Activity export selects and redacts evidence from the provider's shared activity reader.
use crate::query_store::decode;
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, NativeStatement, ReadOptions},
    conn::Pool,
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeSet, sync::Arc};
const MAX_ACTIVITY_SQL_BYTES: u32 = 16 * 1024;
pub(super) struct ActivitySource {
    inner: culprit_read::FrozenSource<NativeActivityReader>,
}
struct NativeActivityReader {
    reader: ActivityReader,
}
impl ActivitySource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        };
        let reader = NativeActivityReader { reader: ActivityReader::open(pool, options, host, port)? };
        Ok(Self { inner: culprit_read::FrozenSource::new(reader, request)? })
    }
}
#[async_trait]
impl culprit_read::ObservationReader for NativeActivityReader {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<culprit_read::FrozenObservation> {
        let policy = request.activity_policy().ok_or_else(decode::malformed)?;
        let (description, observations, blocking) = capture(&mut self.reader, policy).await?;
        let records = observations
            .into_iter()
            .map(TelemetryRecord::Activity)
            .chain(blocking.into_iter().map(TelemetryRecord::Blocking))
            .collect();
        Ok(culprit_read::FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        Ok(true)
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
#[async_trait]
impl TelemetrySource for ActivitySource {
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

pub(crate) async fn capture<R: crate::read::NativeRead>(
    reader: &mut ActivityReader<R>,
    policy: &ActivityPolicy,
) -> Result<(CollectionDescription, Vec<ActivityObservation>, Vec<BlockingObservation>)> {
    let mut frame = reader.observe().await?;
    crate::read::require_primary(frame.context.writable)?;
    let selected = ActivityEvidence::select(&frame.observations(), policy)?;
    let keys: BTreeSet<_> = selected.observations.iter().map(|row| row.key.clone()).collect();
    let details = reader.enrich(&mut frame, &keys, false, MAX_ACTIVITY_SQL_BYTES).await?;
    let description = CollectionDescription {
        target: frame.context.database.clone().into(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: frame.context.epoch.ok_or_else(decode::malformed)?,
            at: frame.context.at,
            scope: culprit_core::MeasurementScope::Database,
            complete_scope: true,
            policy: policy.clone(),
            native_rows: frame.records.len() as u32,
            qualifying: selected.conditions.len() as u32,
            uncertain: 0,
            observations: selected.observations.len() as u32,
            blocking_edges: selected.blocking.len() as u32,
            details,
        }),
    };
    let observations = export_observations(frame.records, &keys);
    Ok((description, observations, selected.blocking))
}
pub(crate) fn export_observations(
    records: Vec<crate::activity::ActivityRecord>,
    keys: &BTreeSet<ActivityKey>,
) -> Vec<ActivityObservation> {
    let mut observations: Vec<_> = records
        .into_iter()
        .filter(|record| keys.contains(&record.facts.key))
        .map(|record| {
            let mut observation = record.facts;
            observation.statement = match record.current_sql {
                NativeStatement::Available(text) => DefinitionPayload::sql(&text, culprit_core::Engine::Mssql),
                NativeStatement::Unavailable(reason) => DefinitionPayload::Unavailable { reason },
            };
            observation
        })
        .collect();
    observations.sort_by(|a, b| a.key.cmp(&b.key));
    observations
}
