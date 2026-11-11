//! Activity export selects and redacts evidence from the provider's shared activity reader.
use crate::query_store::decode;
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, NativeStatement, ReadOptions},
    conn::Pool,
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeSet, sync::Arc};
const ENVELOPE_ALLOWANCE: usize = 1024;
const MAX_ACTIVITY_SQL_BYTES: u32 = 16 * 1024;
struct FrozenStream {
    stream: TelemetryStream,
    records: Vec<TelemetryRecord>,
    position: usize,
    finished: bool,
}
impl FrozenStream {
    fn new(stream: TelemetryStream, records: Vec<TelemetryRecord>) -> Self {
        Self { stream, records, position: 0, finished: false }
    }
    fn page(&mut self, cursor: Option<&str>, budget: &CollectionBudget) -> Result<CollectionRead> {
        let expected = (self.position > 0).then(|| self.position.to_string());
        if self.finished || cursor != expected.as_deref() {
            return Err(decode::malformed());
        }
        let mut records = Vec::new();
        let mut bytes = 0;
        for record in self.records.iter().skip(self.position).take(budget.page_rows as usize) {
            let size = serde_json::to_vec(record).map_err(|_| decode::malformed())?.len() + ENVELOPE_ALLOWANCE;
            if bytes + size > budget.page_bytes as usize {
                if records.is_empty() {
                    return Ok(CollectionRead::Stopped(CollectionStop::ByteLimit));
                }
                break;
            }
            bytes += size;
            records.push(record.clone());
        }
        self.position += records.len();
        self.finished = self.position == self.records.len();
        Ok(CollectionRead::Page { records, next_cursor: (!self.finished).then(|| self.position.to_string()) })
    }
}

pub(super) struct ActivitySource {
    request: CollectionRequest,
    reader: ActivityReader,
    description: Option<CollectionDescription>,
    streams: Vec<FrozenStream>,
    progress: super::progress::PagedProgress,
}
impl ActivitySource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        let options = ReadOptions {
            budget: request.budget.clone(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        };
        Ok(Self {
            request: request.clone(),
            reader: ActivityReader::open(pool, options, host, port)?,
            description: None,
            streams: Vec::new(),
            progress: super::progress::PagedProgress::new(request.collection()),
        })
    }
}
pub(crate) async fn capture<R: crate::read::NativeRead>(
    reader: &mut ActivityReader<R>,
    policy: &ActivityPolicy,
) -> Result<(CollectionDescription, Vec<ActivityObservation>, Vec<BlockingObservation>)> {
    let mut frame = reader.observe().await?;
    super::description::require_primary(frame.context.writable)?;
    let selected = ActivityEvidence::select(&frame.observations(), policy)?;
    let keys: BTreeSet<_> = selected.observations.iter().map(|row| row.key.clone()).collect();
    let details = reader.enrich(&mut frame, &keys, false, MAX_ACTIVITY_SQL_BYTES).await?;
    let description = CollectionDescription {
        database: frame.context.database.clone(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: frame.context.epoch.ok_or_else(decode::malformed)?,
            at: frame.context.at,
            database_wide: true,
            policy: policy.clone(),
            native_rows: frame.records.len() as u32,
            qualifying: selected.conditions.len() as u32,
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
#[async_trait]
impl TelemetrySource for ActivitySource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        let policy = self.request.activity_policy().ok_or_else(decode::malformed)?;
        let (description, observations, blocking) = capture(&mut self.reader, policy).await?;
        if !description.matches_request(&self.request) {
            return Err(decode::malformed());
        }
        self.streams = vec![
            FrozenStream::new(
                TelemetryStream::ActivityObservations,
                observations.into_iter().map(TelemetryRecord::Activity).collect(),
            ),
            FrozenStream::new(
                TelemetryStream::BlockingEdges,
                blocking.into_iter().map(TelemetryRecord::Blocking).collect(),
            ),
        ];
        self.description = Some(description.clone());
        self.progress.described();
        Ok(description)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    fn query_count(&self) -> u32 {
        self.reader.query_count()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.progress.reading(stream, cursor)?;
        let result = self
            .streams
            .iter_mut()
            .find(|family| family.stream == stream)
            .ok_or_else(decode::malformed)?
            .page(cursor, &self.request.budget);
        self.progress.read(stream, result)
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        self.progress.verifying()?;
        self.progress.verified();
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(
            self.description.is_some() && self.streams.iter().all(|stream| stream.finished),
        ))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress.close();
        self.reader.close().await
    }
}
#[cfg(test)]
mod tests;
