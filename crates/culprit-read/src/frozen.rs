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
        Ok(SourceCompletion::verified_if(unchanged && self.streams.iter().all(|stream| stream.finished)))
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
