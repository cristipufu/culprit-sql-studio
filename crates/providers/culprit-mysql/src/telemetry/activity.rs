//! The common frozen lifecycle owns delivery; MySQL owns native observations and shutdown proof.
use super::*;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};

struct ActivityReader {
    io: ReadSession,
    connector: Arc<Connector>,
    metadata: Option<Metadata>,
}
pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    crate::probe::database_of(&connector)?;
    Ok(Box::new(FrozenSource::new(
        ActivityReader {
            io: ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server),
            connector,
            metadata: None,
        },
        request,
    )?))
}
#[async_trait::async_trait]
impl ObservationReader for ActivityReader {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let policy = request.activity_policy().ok_or_else(native::invalid)?;
        let meta = metadata::activity(&mut self.io, &self.connector).await?;
        let frame = native::activity::observe(&mut self.io, &meta, &request.budget, policy).await?;
        let mut evidence = ActivityEvidence::select(&frame.observations, policy)?;
        let cap = (request.budget.page_bytes as usize / evidence.observations.len().max(1))
            .saturating_sub(1024)
            .min(request.budget.max_sql_bytes as usize)
            .min(16 * 1024);
        native::activity::enrich(&mut self.io, &mut evidence.observations, cap).await?;
        let description = CollectionDescription {
            target: meta.connection.database.clone().into(),
            source: "mysql-persistent-activity".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: digest(
                    format!(
                        "{}|{}|{}|{}",
                        meta.connection.server_id,
                        meta.connection.server_name,
                        meta.connection.version,
                        meta.started_second.ok_or_else(native::invalid)?
                    )
                    .as_bytes(),
                ),
                at: meta.connection.at.clone(),
                scope: MeasurementScope::Database,
                complete_scope: frame.complete,
                policy: policy.clone(),
                native_rows: frame.native_rows,
                qualifying: evidence.conditions.len() as u32,
                uncertain: 0,
                observations: evidence.observations.len() as u32,
                blocking_edges: evidence.blocking.len() as u32,
                details: frame.details,
            }),
        };
        self.metadata = Some(meta);
        Ok(FrozenObservation {
            description,
            records: evidence
                .observations
                .into_iter()
                .map(TelemetryRecord::Activity)
                .chain(evidence.blocking.into_iter().map(TelemetryRecord::Blocking))
                .collect(),
        })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let after = metadata::activity(&mut self.io, &self.connector).await?;
        Ok(self.metadata.as_ref().is_some_and(|before| before.same_source(&after)))
    }
    fn query_count(&self) -> u32 {
        self.io.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
