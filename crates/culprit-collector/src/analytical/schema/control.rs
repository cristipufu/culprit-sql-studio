//! One nonrecursive completion row per verified capture, including explicit successful-empty coverage.
use super::*;
use ScalarType::*;
pub fn published_ranges() -> Result<LogicalSchema> {
    LogicalSchema::new(
        "CulpritPublishedRangesV1",
        [
            Field::required("ContractId", "/_culprit/contract_id", String),
            Field::required("ManifestSha256", "/_culprit/manifest_sha256", String),
            Field::required("SchemaDigest", "/_culprit/schema_digest", String),
            Field::required("RecoveryStoreId", "/recovery_store_id", String),
            Field::required("ReceiptsSha256", "/receipts_sha256", String),
            Field::required("PublicationId", "/publication_id", String),
            Field::required("DatabaseId", "/_culprit/database_id", String),
            Field::required("SourceId", "/_culprit/source_id", String),
            Field::required("LogicalSourceId", "/_culprit/logical_source_id", String),
            Field::required("SourceName", "/_culprit/source_name", String),
            Field::required("Engine", "/_culprit/engine", String),
            Field::required("RangeId", "/_culprit/range_id", String),
            Field::required("RangeKind", "/_culprit/range_kind", String),
            Field::optional("Dataset", "/_culprit/dataset", String),
            Field::required("SourceContinuity", "/_culprit/continuity", String),
            Field::required("ObservationId", "/capture/observation_id", String),
            Field::required("OutboxId", "/capture/revision/outbox_id", String),
            Field::required("OwnerEpoch", "/capture/revision/owner_epoch", Long),
            Field::required("CaptureSequence", "/capture/revision/sequence", Long),
            Field::required("CollectedFrom", "/_culprit/collected_from", Timestamp),
            Field::required("CollectedUntil", "/_culprit/collected_until", Timestamp),
            Field::optional("IntervalId", "/_culprit/interval_id", String),
            Field::optional("IntervalFrom", "/_culprit/interval_from", Timestamp),
            Field::optional("IntervalUntil", "/_culprit/interval_until", Timestamp),
            Field::optional("ObservedAt", "/_culprit/observed_at", Timestamp),
            Field::optional("ObservedFrom", "/_culprit/observed_from", Timestamp),
            Field::optional("ObservedUntil", "/_culprit/observed_until", Timestamp),
            Field::required("SourceDescription", "/source", JsonContainer),
            Field::optional("ReadMetrics", "/read_metrics", JsonContainer),
            Field::required("Batches", "/batches", JsonContainer),
        ],
    )
}
