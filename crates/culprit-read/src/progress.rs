//! Snapshot ownership survives retired futures: only a new owner may describe, and only a drained owner may verify.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceProgress {
    New,
    Ready,
    Reading,
    Complete,
    Failed,
    Closed,
    Finished,
}

use crate::invalid_source;
use culprit_core::{
    Result,
    telemetry::{CollectionRead, TelemetryCollection, TelemetryStream},
};

/// Owns stream positions and verification admission across asynchronous history reads.
pub struct PagedProgress {
    phase: SourceProgress,
    streams: Vec<StreamPosition>,
}
struct StreamPosition {
    stream: TelemetryStream,
    cursor: Option<String>,
    drained: bool,
}
impl PagedProgress {
    pub fn new(collection: TelemetryCollection) -> Self {
        Self {
            phase: SourceProgress::New,
            streams: collection
                .source_streams()
                .iter()
                .map(|stream| StreamPosition { stream: *stream, cursor: None, drained: false })
                .collect(),
        }
    }
    pub fn describing(&mut self) -> Result<()> {
        if self.phase != SourceProgress::New {
            return Err(invalid_source());
        }
        self.phase = SourceProgress::Failed;
        Ok(())
    }
    pub fn described(&mut self) {
        self.phase = SourceProgress::Ready;
    }
    pub fn reading(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<()> {
        if !matches!(self.phase, SourceProgress::Ready | SourceProgress::Reading) {
            return Err(invalid_source());
        }
        self.phase = SourceProgress::Failed; // A dropped future retains ownership until native close.
        if !self
            .streams
            .iter()
            .any(|position| position.stream == stream && !position.drained && position.cursor.as_deref() == cursor)
        {
            return Err(invalid_source());
        }
        Ok(())
    }
    pub fn read(&mut self, stream: TelemetryStream, result: Result<CollectionRead>) -> Result<CollectionRead> {
        if let Ok(CollectionRead::Page { next_cursor, .. }) = &result {
            let position =
                self.streams.iter_mut().find(|position| position.stream == stream).ok_or_else(invalid_source)?;
            position.cursor = next_cursor.clone();
            position.drained = next_cursor.is_none();
            self.phase = if self.streams.iter().all(|position| position.drained) {
                SourceProgress::Complete
            } else {
                SourceProgress::Reading
            };
        }
        result
    }
    pub fn verifying(&mut self) -> Result<()> {
        if self.phase != SourceProgress::Complete {
            return Err(invalid_source());
        }
        self.phase = SourceProgress::Failed;
        Ok(())
    }
    pub fn verified(&mut self) {
        self.phase = SourceProgress::Finished;
    }
    pub fn close(&mut self) {
        self.phase = SourceProgress::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(next: Option<&str>) -> Result<CollectionRead> {
        Ok(CollectionRead::Page { records: vec![], next_cursor: next.map(String::from) })
    }
    #[test]
    fn only_drained_streams_authorize_one_verification() {
        let mut progress = PagedProgress::new(TelemetryCollection::QueryHistory);
        assert!(progress.verifying().is_err());
        progress.describing().unwrap();
        progress.described();
        for stream in TelemetryCollection::QueryHistory.source_streams() {
            assert!(progress.verifying().is_err());
            progress.reading(*stream, None).unwrap();
            progress.read(*stream, page(None)).unwrap();
        }
        progress.verifying().unwrap();
        progress.verified();
        assert!(progress.verifying().is_err());
        assert!(progress.reading(TelemetryStream::QueryRuntime, None).is_err());
    }
    #[test]
    fn retired_description_or_page_requires_close_and_cannot_reopen() {
        let mut description = PagedProgress::new(TelemetryCollection::ResourceHistory);
        description.describing().unwrap();
        assert!(description.describing().is_err());
        description.close();
        assert!(description.describing().is_err());
        let mut progress = PagedProgress::new(TelemetryCollection::ResourceHistory);
        progress.describing().unwrap();
        progress.described();
        progress.reading(TelemetryStream::ResourceSamples, None).unwrap();
        assert!(progress.reading(TelemetryStream::ResourceSamples, None).is_err());
        assert!(progress.verifying().is_err());
        progress.close();
        assert!(progress.reading(TelemetryStream::ResourceSamples, None).is_err());
    }
    #[test]
    fn cursor_replay_or_source_failure_retires_the_observation() {
        for fail_native in [false, true] {
            let mut progress = PagedProgress::new(TelemetryCollection::ResourceHistory);
            progress.describing().unwrap();
            progress.described();
            progress.reading(TelemetryStream::ResourceSamples, None).unwrap();
            progress.read(TelemetryStream::ResourceSamples, page(Some("next"))).unwrap();
            if fail_native {
                progress.reading(TelemetryStream::ResourceSamples, Some("next")).unwrap();
                assert!(progress.read(TelemetryStream::ResourceSamples, Err(invalid_source())).is_err());
            } else {
                assert!(progress.reading(TelemetryStream::ResourceSamples, None).is_err());
            }
            assert!(progress.reading(TelemetryStream::ResourceSamples, Some("next")).is_err());
            assert!(progress.verifying().is_err());
        }
    }
}
