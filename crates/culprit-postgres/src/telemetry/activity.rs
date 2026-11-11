//! Shared frozen-frame delivery over PostgreSQL's native activity reader.
use super::*;
use crate::native::activity;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::collections::BTreeSet;
struct ActivityReader {
    io: ReadSession,
    host: String,
    port: u16,
    metadata: Option<Metadata>,
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    let reader = ActivityReader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        metadata: None,
    };
    Ok(Box::new(FrozenSource::new(reader, request)?))
}
#[async_trait::async_trait]
impl ObservationReader for ActivityReader {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let policy = request.activity_policy().ok_or_else(native::invalid)?;
        let meta = metadata::read(&mut self.io, &self.host, self.port).await?;
        if meta.version < 140_000 {
            return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
        }
        if !meta.full_visibility {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        let mut frame = activity::observe(&mut self.io, &meta, &request.budget).await?;
        if !frame.fully_visible(&meta) {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        activity::blockers(
            &mut self.io,
            &mut frame,
            activity::BlockingReadOptions {
                minimum_request_age_ms: policy.blocked_after_ms,
                scope: activity::ActivityScope::CurrentDatabase,
                wait_start_available: true,
            },
        )
        .await?;
        let native_rows = frame.records.len() as u32;
        let selected = ActivityEvidence::select(&frame.observations(), policy)?;
        let keys: BTreeSet<_> = selected.observations.iter().map(|row| row.key.clone()).collect();
        let cap = (request.budget.page_bytes as usize / keys.len().max(1))
            .saturating_sub(1024)
            .min(request.budget.max_sql_bytes as usize)
            .min(16 * 1024);
        let details = activity::enrich(&mut self.io, &mut frame, &keys, cap, false).await?;
        let observations: Vec<_> =
            frame.records.into_iter().filter_map(|row| row.facts).filter(|row| keys.contains(&row.key)).collect();
        let description = CollectionDescription {
            database: meta.database.clone(),
            source: "pg_stat_activity".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: digest(
                    format!("{}|{}|{}|pg-activity-v1", meta.database_oid, meta.started_at, meta.version).as_bytes(),
                ),
                at: frame.at,
                scope: culprit_core::MeasurementScope::Database,
                complete_scope: true,
                policy: policy.clone(),
                native_rows,
                qualifying: selected.conditions.len() as u32,
                uncertain: 0,
                observations: observations.len() as u32,
                blocking_edges: selected.blocking.len() as u32,
                details,
            }),
        };
        self.metadata = Some(meta);
        let records = observations
            .into_iter()
            .map(TelemetryRecord::Activity)
            .chain(selected.blocking.into_iter().map(TelemetryRecord::Blocking))
            .collect();
        Ok(FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let after = metadata::read(&mut self.io, &self.host, self.port).await?;
        Ok(self.metadata.as_ref().is_some_and(|before| before.same_source(&after)))
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
