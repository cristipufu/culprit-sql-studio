//! A bounded retained-event observation, using the same native reader/parser as interactive deadlocks.
use super::description;
use crate::{
    conn::Pool,
    deadlocks::native,
    read::{NativeRead, ReadError, ReadSession, invalid_data, text},
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;
const ENVELOPE_ALLOWANCE: usize = 4096;

pub(super) struct EventSource<R = ReadSession> {
    request: CollectionRequest,
    host: String,
    port: u16,
    io: R,
    description: Option<CollectionDescription>,
    physical: Option<String>,
    positions: Option<Vec<String>>,
    records: Vec<TelemetryRecord>,
    offset: usize,
    progress: super::progress::PagedProgress,
}
impl EventSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(ReadSession::new(pool, request.budget.clone()), request, host, port)
    }
}
impl<R: NativeRead> EventSource<R> {
    fn with_io(io: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        if request.collection() != TelemetryCollection::DeadlockHistory {
            return Err(invalid_data());
        }
        Ok(Self {
            request: request.clone(),
            host,
            port,
            io,
            description: None,
            physical: None,
            positions: None,
            records: vec![],
            offset: 0,
            progress: super::progress::PagedProgress::new(request.collection()),
        })
    }
    async fn page(&mut self) -> Result<CollectionRead> {
        if self.positions.is_none() {
            self.load().await?;
        }
        let mut end = self.offset;
        let mut bytes = 0;
        while end < self.records.len() && end - self.offset < self.request.budget.page_rows as usize {
            let size = serde_json::to_vec(&self.records[end]).map_err(|_| invalid_data())?.len() + ENVELOPE_ALLOWANCE;
            if bytes + size > self.request.budget.page_bytes as usize {
                break;
            }
            bytes += size;
            end += 1;
        }
        if end == self.offset && end < self.records.len() {
            return Ok(CollectionRead::Stopped(CollectionStop::ByteLimit));
        }
        let records = self.records[self.offset..end].to_vec();
        self.offset = end;
        Ok(CollectionRead::Page { records, next_cursor: (end < self.records.len()).then(|| end.to_string()) })
    }

    async fn read_native(&mut self) -> Result<Vec<native::Event>> {
        super::admit_resources(&mut self.io).await.map_err(ReadError::into_error)?;
        let description = self.description.as_ref().ok_or_else(invalid_data)?;
        let (from, until) = self.request.bounds()?;
        let events = native::read(
            &mut self.io,
            native::Origin::DatabaseSession,
            &description.database.database,
            &from.to_rfc3339(),
            &until.to_rfc3339(),
            self.request.budget.max_snapshot_rows as usize + 1,
            false,
        )
        .await?;
        if events.len() > self.request.budget.max_snapshot_rows as usize {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(events)
    }
    async fn load(&mut self) -> Result<()> {
        let events = self.read_native().await?;
        self.positions = Some(events.iter().map(|event| event.position.clone()).collect());
        for event in events {
            for (ordinal, graph) in event.graphs.into_iter().enumerate() {
                let mut row = DeadlockEvent {
                    position: event.position.clone(),
                    file_id: event.file_id.clone(),
                    file_offset: event.offset.clone(),
                    graph_ordinal: ordinal as u32,
                    at: event.at.clone(),
                    processes: graph.processes,
                    resources: graph.resources,
                    pattern: graph.pattern,
                };
                row.redact(culprit_core::Engine::Mssql);
                self.records.push(TelemetryRecord::Deadlock(row));
            }
        }
        if self.records.len() > self.request.budget.max_snapshot_rows as usize {
            return Err(CollectionStop::RowLimit.error());
        }
        Ok(())
    }
}
#[async_trait]
impl<R: NativeRead> TelemetrySource for EventSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        description::ensure_supported(&mut self.io).await?;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        let server = self.io.query(native::SERVER, vec![], 1).await.map_err(ReadError::into_error)?;
        let at = canonical_time(&text(server.first().ok_or_else(invalid_data)?, "observed_at")?)?;
        let (from, until) = self.request.bounds()?;
        let value = CollectionDescription {
            database,
            source: culprit_core::source_id::DEADLOCKS.into(),
            observation: SourceObservation::RetainedEvents(EventHistoryDescription {
                from: from.to_rfc3339(),
                until: until.to_rfc3339(),
                observed_at: at,
                retained_from: None,
                history_complete: false,
            }),
            continuity: SourceContinuity::Unverified,
        };
        if !value.matches_request(&self.request) {
            return Err(invalid_data());
        }
        self.physical = Some(physical);
        self.description = Some(value.clone());
        self.progress.described();
        Ok(value)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.progress.reading(stream, cursor)?;
        let result = self.page().await;
        self.progress.read(stream, result)
    }

    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        self.progress.verifying()?;
        let (database, physical) = description::identity(&mut self.io, &self.host, self.port).await?;
        if self.physical.as_ref() != Some(&physical)
            || self.description.as_ref().is_none_or(|description| description.database != database)
        {
            self.progress.verified();
            return Ok(culprit_core::telemetry::SourceCompletion::Changed);
        }
        let events = self.read_native().await?;
        self.progress.verified();
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(
            self.positions.as_ref() == Some(&events.into_iter().map(|event| event.position).collect::<Vec<_>>()),
        ))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress.close();
        self.io.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::{FakeRead, latest_row, replace},
        value::{Rec, Val},
    };
    fn request() -> CollectionRequest {
        CollectionRequest {
            selection: CollectionSelection::DeadlockHistory {
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T06:01:00Z".into(),
            },
            budget: CollectionBudget { page_rows: 1, ..Default::default() },
        }
    }
    fn identity() -> Rec {
        Rec::of(&[
            ("database_name", Val::Text("db".into())),
            ("incarnation", Val::Text("logical".into())),
            ("physical_incarnation", Val::Text("physical".into())),
            ("updateability", Val::Text("READ_WRITE".into())),
        ])
    }
    fn server() -> Rec {
        Rec::of(&[
            ("edition", Val::Int(5)),
            ("db", Val::Text("db".into())),
            ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ])
    }
    fn event(offset: u64) -> String {
        crate::deadlocks::database_test_support::event(offset)
    }
    fn pressure() -> Rec {
        replace(
            &latest_row("2020-01-01T06:01:45Z"),
            &[
                ("avg_instance_cpu_percent", Val::Float(10.0)),
                ("max_worker_percent", Val::Float(10.0)),
                ("avg_log_write_percent", Val::Float(10.0)),
            ],
        )
    }
    fn replies(events: Vec<String>, verified: Vec<String>) -> FakeRead {
        use crate::deadlocks::database_test_support::{configuration, target};
        FakeRead::new(vec![
            Ok(vec![server()]),
            Ok(vec![identity()]),
            Ok(vec![server()]),
            Ok(vec![pressure()]),
            Ok(vec![configuration()]),
            Ok(vec![target(&events)]),
            Ok(vec![identity()]),
            Ok(vec![pressure()]),
            Ok(vec![configuration()]),
            Ok(vec![target(&verified)]),
        ])
    }
    #[tokio::test]
    async fn bounded_event_pages_recheck_native_membership_and_never_claim_complete_retention() {
        let mut source = EventSource::with_io(
            replies(vec![event(1), event(2)], vec![event(1), event(2)]),
            &request(),
            "host".into(),
            1433,
        )
        .unwrap();
        let description = source.describe().await.unwrap();
        assert!(description.matches_request(&request()));
        assert!(!description.observation.events().unwrap().history_complete);
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::DeadlockEvents, None).await.unwrap()
        else {
            panic!("page")
        };
        assert_eq!(records.len(), 1);
        let encoded = serde_json::to_string(&records).unwrap();
        for canary in ["private-deadlock-canary", "private-host", "private-user"] {
            assert!(!encoded.contains(canary));
        }
        let row = ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "observation".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        assert!(row.record_id.is_some());
        assert!(next_cursor.is_some());
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::DeadlockEvents, next_cursor.as_deref()).await.unwrap()
        else {
            panic!("page")
        };
        assert_eq!(records.len(), 1);
        assert!(next_cursor.is_none());
        assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    #[tokio::test]
    async fn late_or_changed_events_prevent_complete_observation_commit() {
        let mut source =
            EventSource::with_io(replies(vec![event(1)], vec![event(1), event(2)]), &request(), "host".into(), 1433)
                .unwrap();
        source.describe().await.unwrap();
        source.read_page(TelemetryStream::DeadlockEvents, None).await.unwrap();
        assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Changed);
    }
    #[tokio::test]
    async fn exceeding_the_native_event_cap_does_not_turn_latest_n_into_successful_history() {
        let mut request = request();
        request.budget.max_snapshot_rows = 1;
        let mut source =
            EventSource::with_io(replies(vec![event(1), event(2)], vec![]), &request, "host".into(), 1433).unwrap();
        source.describe().await.unwrap();
        let error = source.read_page(TelemetryStream::DeadlockEvents, None).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
        assert!(source.finish().await.is_err());
    }

    #[tokio::test]
    async fn cancelling_a_target_read_retains_the_native_owner_until_explicit_close() {
        let io = replies(vec![event(1)], vec![]).pause_on_query(6);
        let mut source = EventSource::with_io(io, &request(), "host".into(), 1433).unwrap();
        source.describe().await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                source.read_page(TelemetryStream::DeadlockEvents, None),
            )
            .await
            .is_err()
        );
        assert_eq!(source.io.calls.len(), 6);
        assert_eq!(source.io.closed, 0);
        source.io.shutdown = SourceShutdown::Unconfirmed;
        assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
        assert_eq!(source.io.closed, 1);
        assert!(source.read_page(TelemetryStream::DeadlockEvents, None).await.is_err());
        assert_eq!(source.io.calls.len(), 6);
    }
}
