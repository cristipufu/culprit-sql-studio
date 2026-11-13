use super::*;
use crate::{
    analytical::{
        ProjectionCatalog, RowProjection,
        catalog::PhysicalCatalog,
        indexes::ProjectionIndexes,
        projection, publication,
        read_store::{AnalyticalReadPage, AnalyticalReadStore},
        schema::{LogicalSchema, OutputColumn},
    },
    delivery::recovery::CommittedRange,
};
use async_trait::async_trait;
use culprit_core::{Result, SourceState};
use serde_json::{Map, Value};
use std::sync::Arc;

/// The fixture's warehouse returns already normalized logical values; no native dialect is being emulated.
struct LogicalProjection {
    schema: LogicalSchema,
    columns: Vec<OutputColumn>,
}
impl RowProjection for LogicalProjection {
    fn from_logical(schema: LogicalSchema) -> Result<Self> {
        Ok(Self { columns: schema.columns(), schema })
    }
    fn table(&self) -> &str {
        self.schema.table()
    }
    fn digest(&self) -> &str {
        self.schema.digest()
    }
    fn columns(&self) -> &[OutputColumn] {
        &self.columns
    }
    fn project(&self, source: &Value) -> Result<Vec<Value>> {
        self.schema.project(source)
    }
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>> {
        self.schema.encode(values)
    }
    fn returned_digest(&self, values: &[Value]) -> Result<[u8; 32]> {
        projection::digest(values)
    }
}
struct EmptyStore {
    catalog: Arc<PhysicalCatalog<LogicalProjection>>,
    published: bool,
}
#[async_trait]
impl AnalyticalReadStore for EmptyStore {
    fn destination(&self) -> &str {
        "fixture:test"
    }
    fn catalog(&self) -> Arc<dyn ProjectionCatalog> {
        self.catalog.clone()
    }
    fn logical_values(&self, _: TelemetryStream, _: &[Value]) -> Result<Map<String, Value>> {
        panic!("an authenticated zero-row snapshot has no native row to reconstruct")
    }
    async fn audit_membership(&self, range: &CommittedRange, _: usize) -> Result<usize> {
        assert!(range.manifest().batches.iter().all(|batch| batch.rows.is_empty()));
        Ok(1)
    }
    async fn rows(&self, _: &CommittedRange, _: &str, _: usize, _: usize, _: usize) -> Result<AnalyticalReadPage> {
        panic!("a declared zero-row batch must not query native rows")
    }
    async fn publication_rows(&self, range: &CommittedRange, _: usize) -> Result<AnalyticalReadPage> {
        let rows = if self.published {
            vec![self.catalog.publication().project(&publication::input(self.catalog.as_ref(), range)?)?]
        } else {
            vec![]
        };
        Ok(AnalyticalReadPage { response_bytes: serde_json::to_vec(&rows).unwrap().len(), rows })
    }
}

async fn empty_capture(
    dataset: SnapshotDataset,
    coverage: StateSourceCoverage,
    published: bool,
    families: &[EvidenceFamily],
) -> EvidenceCapture {
    let (mut source, _) = test_support::memory_failure_evidence();
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.dataset = dataset;
    snapshot.records = 0;
    snapshot.sources = vec![coverage];
    let catalog = Arc::new(PhysicalCatalog::<LogicalProjection>::new().unwrap());
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), vec![], 150_000);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    assert_eq!(range.manifest().batches.len(), 1);
    assert!(range.manifest().batches[0].rows.is_empty());
    assert!(range.manifest().batches[0].delivery_required);
    *recovery.fault.lock().unwrap() = test_support::Fault::Stall;
    let reader = EvidenceReader::new(
        Arc::new(EmptyStore { catalog, published }),
        Arc::new(ProjectionIndexes::new(Arc::new(test_support::Blobs::default()))),
    );
    let result = reader.read_capture(&range, families, EvidenceReadBudget::default()).await.unwrap();
    assert_eq!(result.source(), &source);
    assert!(result.records().is_empty());
    assert_eq!(result.metrics().pages, 0);
    assert_eq!(result.metrics().proof_bytes, 0);
    result
}

#[tokio::test]
async fn published_zero_row_snapshots_preserve_empty_unavailable_and_partial_native_coverage() {
    for dataset in [SnapshotDataset::MemoryFailures, SnapshotDataset::Cleanup, SnapshotDataset::Operational] {
        for coverage in [
            StateSourceCoverage::collected("native-buffer".into(), 0),
            StateSourceCoverage::unavailable("native-buffer".into(), culprit_core::ErrorCode::PermissionDenied, None),
            StateSourceCoverage::observed("native-buffer".into(), 0, false),
        ] {
            let result = empty_capture(dataset, coverage.clone(), true, &[EvidenceFamily::OperationalSamples]).await;
            assert!(result.is_read_complete(), "{:?}", result.gaps());
            assert_eq!(result.source().observation.snapshot().unwrap().sources, vec![coverage]);
            assert_eq!(result.metrics().queries, 2);
        }
    }
}

#[tokio::test]
async fn a_published_empty_snapshot_does_not_establish_capture_of_another_family() {
    let result = empty_capture(
        SnapshotDataset::MemoryFailures,
        StateSourceCoverage::collected("native-buffer".into(), 0),
        true,
        &[EvidenceFamily::OperationalSamples, EvidenceFamily::ResourceSamples],
    )
    .await;
    assert!(!result.is_read_complete());
    assert_eq!(result.gaps().len(), 1);
    assert_eq!(result.gaps()[0].stream, Some(TelemetryStream::ResourceSamples));
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::FamilyNotCaptured);
    assert_eq!(result.source().observation.snapshot().unwrap().sources[0].state, SourceState::Empty);
}

#[tokio::test]
async fn an_unpublished_empty_snapshot_remains_unpublished_evidence() {
    let result = empty_capture(
        SnapshotDataset::MemoryFailures,
        StateSourceCoverage::collected("native-buffer".into(), 0),
        false,
        &[EvidenceFamily::OperationalSamples],
    )
    .await;
    assert!(!result.is_read_complete());
    assert_eq!(result.gaps().len(), 1);
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::NotPublished);
    assert_eq!(result.metrics().queries, 1);
}
