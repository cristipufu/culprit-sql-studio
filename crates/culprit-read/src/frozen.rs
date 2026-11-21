//! Paging an already observed, bounded frame never knows a native driver or a database family.
use crate::{PagedProgress, invalid_source};
use culprit_core::{Result, telemetry::*};

const ENVELOPE_ALLOWANCE: usize = 1024;
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
            return Err(invalid_source());
        }
        let mut records = Vec::new();
        let mut bytes = 0;
        for record in self.records.iter().skip(self.position).take(budget.page_rows as usize) {
            let size = serde_json::to_vec(record).map_err(|_| invalid_source())?.len() + ENVELOPE_ALLOWANCE;
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
pub struct FrozenObservation {
    pub description: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
}
#[async_trait::async_trait]
pub trait ObservationReader: Send {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation>;
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool>;
    fn query_count(&self) -> u32;
    fn read_metrics(&self) -> Option<ReadWorkMetrics>;
    async fn close(&mut self) -> SourceShutdown;
}
pub struct FrozenSource<R: ObservationReader> {
    reader: R,
    request: CollectionRequest,
    description: Option<CollectionDescription>,
    streams: Vec<FrozenStream>,
    progress: PagedProgress,
}
impl<R: ObservationReader> FrozenSource<R> {
    pub fn new(reader: R, request: &CollectionRequest) -> Result<Self> {
        request.validate()?;
        Ok(Self {
            reader,
            request: request.clone(),
            description: None,
            streams: vec![],
            progress: PagedProgress::new(request.collection()),
        })
    }
}
#[async_trait::async_trait]
impl<R: ObservationReader> TelemetrySource for FrozenSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        let frame = self.reader.observe(&self.request).await?;
        if !frame.description.matches_request(&self.request) {
            return Err(invalid_source());
        }
        let mut streams: Vec<_> = self
            .request
            .collection()
            .source_streams()
            .iter()
            .map(|stream| FrozenStream::new(*stream, vec![]))
            .collect();
        for record in frame.records {
            let stream =
                streams.iter_mut().find(|stream| stream.stream == record.stream()).ok_or_else(invalid_source)?;
            stream.records.push(record);
        }
        self.streams = streams;
        self.description = Some(frame.description.clone());
        self.progress.described();
        Ok(frame.description)
    }
    fn query_count(&self) -> u32 {
        self.reader.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.reader.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.progress.reading(stream, cursor)?;
        let result = self
            .streams
            .iter_mut()
            .find(|family| family.stream == stream)
            .ok_or_else(invalid_source)?
            .page(cursor, &self.request.budget);
        self.progress.read(stream, result)
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.progress.verifying()?;
        let description = self.description.as_ref().ok_or_else(invalid_source)?;
        let unchanged = self.reader.verify(description).await?;
        self.progress.verified();
        if !unchanged || !self.streams.iter().all(|stream| stream.finished) {
            return Ok(SourceCompletion::Changed);
        }
        // A frozen history reader verifies this exact original acquisition header. Keep its typed final
        // witness rather than using generic Verified, which cannot attest an acquisition owner.
        Ok(match &description.observation {
            SourceObservation::QueryHistory(history) if history.acquisition_owner.is_some() => {
                SourceCompletion::QueryHistory(Box::new(history.clone()))
            }
            _ => SourceCompletion::Verified,
        })
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress.close();
        self.reader.close().await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_frozen_stream_finishes_and_rejects_a_second_read() {
        let mut stream = FrozenStream::new(TelemetryStream::ActivityObservations, vec![]);
        assert!(matches!(
            stream.page(None, &CollectionBudget::default()).unwrap(),
            CollectionRead::Page { next_cursor: None, .. }
        ));
        assert!(stream.page(None, &CollectionBudget::default()).is_err());
    }
}

#[cfg(test)]
mod history_completion_tests {
    use super::*;
    struct Reader {
        source: CollectionDescription,
        unchanged: bool,
        reads: u32,
        verifications: u32,
    }
    #[async_trait::async_trait]
    impl ObservationReader for Reader {
        async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
            self.reads += 1;
            Ok(FrozenObservation { description: self.source.clone(), records: vec![] })
        }
        async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
            self.verifications += 1;
            Ok(self.unchanged && source == &self.source)
        }
        fn query_count(&self) -> u32 {
            self.reads + self.verifications
        }
        fn read_metrics(&self) -> Option<ReadWorkMetrics> {
            None
        }
        async fn close(&mut self) -> SourceShutdown {
            SourceShutdown::Quiescent
        }
    }
    fn source(owner: bool, unchanged: bool) -> FrozenSource<Reader> {
        let mut database = culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "controlled-db",
            "fixture.invalid",
            1433,
            None,
        );
        database.incarnation = Some("controlled-logical-database".into());
        let target = CollectionTarget::Database(database);
        let context = NativeReadContext {
            target: target.clone(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("controlled-server"),
                    native: NativeIdentity::new("controlled-member"),
                }),
                epoch: Some("controlled-process".into()),
            },
            observed_from: "2020-01-01T08:00:00Z".into(),
            observed_until: "2020-01-01T08:00:01Z".into(),
            database_writable: Some(true),
        };
        let description = CollectionDescription {
            target,
            partition: SourcePartition::Target,
            source: "controlled-query-history".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::QueryHistory(QueryHistoryDescription {
                interval: TelemetryInterval {
                    native_id: "original-interval".into(),
                    from: "2020-01-01T06:00:00Z".into(),
                    until: "2020-01-01T07:00:00Z".into(),
                },
                acquisition_owner: owner.then_some(Box::new(context)),
                population_catalog: None,
                population_binding: QueryHistoryPopulationBinding::Unknown,
                waits: None,
                dimensions: Default::default(),
                native_properties: Default::default(),
            }),
        };
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::QueryHistory {
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            budget: CollectionBudget::default(),
        };
        FrozenSource::new(Reader { source: description, unchanged, reads: 0, verifications: 0 }, &request).unwrap()
    }
    async fn drain(source: &mut FrozenSource<Reader>) {
        for stream in TelemetryCollection::QueryHistory.source_streams() {
            assert!(matches!(
                source.read_page(*stream, None).await.unwrap(),
                CollectionRead::Page { next_cursor: None, .. }
            ));
        }
    }
    #[tokio::test]
    async fn all_four_original_streams_and_the_same_reader_verify_preserve_the_exact_typed_history_witness() {
        for owner in [false, true] {
            let mut source = source(owner, true);
            let original = source.describe().await.unwrap();
            if owner {
                assert!(original.completed(SourceCompletion::Verified).is_err());
            }
            drain(&mut source).await;
            let completion = source.finish().await.unwrap();
            assert_eq!(matches!(completion, SourceCompletion::QueryHistory(_)), owner);
            assert_eq!(original.completed(completion).unwrap(), Some(original.clone()));
            assert_eq!(source.query_count(), 2);
            assert_eq!(source.close().await, SourceShutdown::Quiescent);
        }
    }
    #[tokio::test]
    async fn missing_stream_or_changed_original_never_produces_a_verified_history_header() {
        let mut undrained = source(true, true);
        undrained.describe().await.unwrap();
        undrained.read_page(TelemetryStream::QueryRuntime, None).await.unwrap();
        assert!(undrained.finish().await.is_err());
        assert_eq!(undrained.reader.verifications, 0);
        let mut changed = source(true, false);
        let original = changed.describe().await.unwrap();
        drain(&mut changed).await;
        let completion = changed.finish().await.unwrap();
        assert!(!completion.is_verified());
        assert_eq!(original.completed(completion).unwrap(), None);
        assert_eq!(changed.reader.verifications, 1);
    }
}
