//! InnoDB's latest report is a retained buffer, never a complete history. Native row contents are not exported.
use super::*;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
struct Reader {
    connector: Arc<Connector>,
    io: ReadSession,
    metadata: Option<Metadata>,
}
pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    crate::probe::database_of(&connector)?;
    Ok(Box::new(FrozenSource::new(
        Reader {
            io: ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server),
            connector,
            metadata: None,
        },
        request,
    )?))
}
#[async_trait::async_trait]
impl ObservationReader for Reader {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        use crate::read::QueryRead;
        let CollectionSelection::DeadlockHistory { from, until } = &request.selection else {
            return Err(native::invalid());
        };
        let meta = metadata::read(&mut self.io, &self.connector).await?;
        let rows = self.io.read(crate::deadlocks::LATEST_SQL, vec![], 2).await?;
        if rows.len() != 1 {
            return Err(native::invalid());
        }
        let status = native::text(&rows[0], "status")?;
        let mut records = vec![];
        if let Some(buffer) = crate::deadlocks::latest_section(&status) {
            let zones = self.io.read("SELECT @@system_time_zone AS system_zone", vec![], 1).await?;
            let utc = zones
                .first()
                .map(|row| native::text(row, "system_zone"))
                .transpose()?
                .is_some_and(|zone| matches!(zone.as_str(), "UTC" | "GMT"));
            // A local time in a repeated DST hour is ambiguous. Do not turn today's UTC offset into historical proof.
            if !utc {
                return Err(CollectionStop::SourceUnavailable.error());
            }
            let databases = crate::deadlocks::native_databases(buffer)?;
            if databases.contains(&meta.connection.database_name) {
                for (ordinal, graph) in crate::deadlocks::parse_innodb_status(buffer, true).into_iter().enumerate() {
                    let at = graph.at.ok_or_else(native::invalid)?;
                    let instant = culprit_core::format::epoch_seconds(&at).ok_or_else(native::invalid)?;
                    let start = culprit_core::format::epoch_seconds(from).ok_or_else(native::invalid)?;
                    let end = culprit_core::format::epoch_seconds(until).ok_or_else(native::invalid)?;
                    if !(start <= instant && instant < end) {
                        continue;
                    }
                    let mut event = DeadlockEvent {
                        position: digest(buffer.as_bytes()),
                        file_id: digest(b"innodb-status-latest-deadlock-buffer"),
                        // Actual byte offset in the native SHOW ENGINE response; file_id names the retained buffer.
                        file_offset: status.find(buffer).ok_or_else(native::invalid)?.to_string(),
                        graph_ordinal: ordinal as u32,
                        at,
                        processes: graph.processes,
                        resources: graph.resources,
                        pattern: graph.pattern,
                    };
                    event.redact(culprit_core::Engine::Mysql);
                    if !event.is_valid() {
                        return Err(native::invalid());
                    }
                    records.push(TelemetryRecord::Deadlock(event));
                }
            }
        }
        let after = metadata::read(&mut self.io, &self.connector).await?;
        if !meta.same_source(&after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let description = CollectionDescription {
            target: meta.connection.database.clone().into(),
            source: "innodb-latest-deadlock".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::RetainedEvents(EventHistoryDescription {
                from: from.clone(),
                until: until.clone(),
                observed_at: after.connection.at,
                retained_from: None,
                history_complete: false,
            }),
        };
        self.metadata = Some(meta);
        Ok(FrozenObservation { description, records })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let after = metadata::read(&mut self.io, &self.connector).await?;
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
