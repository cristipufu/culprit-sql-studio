//! Destination context is derived from the committed native manifest; source artifacts and logical IDs stay intact.
use super::projection::ScalarType::*;
use super::schema::Field;
#[cfg(test)]
mod tests;
use culprit_collector::delivery::evidence::{BatchManifest, RangeManifest, RowReceipt};
use culprit_collector::delivery::recovery::CommittedRange;
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, ExportedRecord, SourceObservation, TelemetryRange},
};
use serde_json::{Value, json};

pub const COMMON: &[Field] = &[
    Field::required("ContractId", "/_culprit/contract_id", String),
    Field::required("ManifestSha256", "/_culprit/manifest_sha256", String),
    Field::required("SchemaDigest", "/_culprit/schema_digest", String),
    Field::required("DatabaseId", "/_culprit/database_id", String),
    Field::required("SourceId", "/_culprit/source_id", String),
    Field::required("LogicalSourceId", "/_culprit/logical_source_id", String),
    Field::required("SourceName", "/_culprit/source_name", String),
    Field::required("Engine", "/_culprit/engine", String),
    Field::required("RangeId", "/_culprit/range_id", String),
    Field::required("RangeKind", "/_culprit/range_kind", String),
    Field::optional("Dataset", "/_culprit/dataset", String),
    Field::required("SourceContinuity", "/_culprit/continuity", String),
    Field::required("PayloadObject", "/_culprit/payload_object", String),
    Field::required("NativePayloadDigest", "/_culprit/native_digest", String),
    Field::required("RowOrdinal", "/_culprit/row_ordinal", Long),
    Field::required("BatchId", "/batch_id", String),
    Field::required("RecordId", "/record_id", String),
    Field::required("ObservationId", "/capture/observation_id", String),
    Field::required("OutboxId", "/capture/revision/outbox_id", String),
    Field::required("OwnerEpoch", "/capture/revision/owner_epoch", Long),
    Field::required("CaptureSequence", "/capture/revision/sequence", Long),
    Field::required("RepresentationVersion", "/representation_version", Long),
    Field::required("FormatVersion", "/format_version", Long),
    Field::required("CollectedFrom", "/_culprit/collected_from", Timestamp),
    Field::required("CollectedUntil", "/_culprit/collected_until", Timestamp),
    Field::optional("IntervalId", "/_culprit/interval_id", String),
    Field::optional("IntervalFrom", "/_culprit/interval_from", Timestamp),
    Field::optional("IntervalUntil", "/_culprit/interval_until", Timestamp),
    Field::optional("ObservedAt", "/_culprit/observed_at", Timestamp),
    Field::optional("ObservedFrom", "/_culprit/observed_from", Timestamp),
    Field::optional("ObservedUntil", "/_culprit/observed_until", Timestamp),
    Field::required("DefinitionPayloads", "/_projection/payloads", JsonContainer),
];

pub struct Envelope<'a> {
    manifest: &'a RangeManifest,
    batch: &'a BatchManifest,
    context: Value,
}
impl<'a> Envelope<'a> {
    pub fn new(manifest: &'a RangeManifest, batch_id: &str) -> Result<Self> {
        manifest.validate()?;
        Self::with_digest(manifest, batch_id, &manifest.digest()?)
    }
    pub(super) fn committed(range: &'a CommittedRange, batch_id: &str) -> Result<Self> {
        Self::with_digest(range.manifest(), batch_id, range.manifest_digest())
    }
    fn with_digest(manifest: &'a RangeManifest, batch_id: &str, manifest_digest: &str) -> Result<Self> {
        let batch = manifest.batches.iter().find(|batch| batch.batch_id == batch_id).ok_or_else(invalid)?;
        let mut context = capture_context(manifest, manifest_digest)?;
        context["payload_object"] = json!(batch.object);
        Ok(Self { manifest, batch, context })
    }
    pub fn input(&self, ordinal: usize, row: &ExportedRecord) -> Result<Value> {
        let expected = ordinal.checked_sub(1).and_then(|index| self.batch.rows.get(index)).ok_or_else(invalid)?;
        let receipt = RowReceipt::from_export(row)?;
        if expected != &receipt || row.capture != self.manifest.capture || row.record.stream() != self.batch.stream {
            return Err(invalid());
        }
        let mut context = self.context.clone();
        context["row_ordinal"] = json!(ordinal);
        context["native_digest"] = json!(receipt.payload_digest);
        let mut value = serde_json::to_value(row).map_err(|_| invalid())?;
        value["_culprit"] = context;
        super::prepared::prepare(&mut value, &row.record)?;
        Ok(value)
    }
}
pub(super) fn capture_context(manifest: &RangeManifest, manifest_digest: &str) -> Result<Value> {
    let source = &manifest.source;
    let captured = source.captured_range()?;
    let mut context = json!({
        "contract_id": manifest.contract.id()?, "manifest_sha256": manifest_digest,
        "schema_digest": manifest.contract.schema_digest(),
        "database_id": source.database.telemetry_id()?, "source_id": captured.scope_id,
        "logical_source_id": source.logical_source_id()?,
        "source_name": source.source, "engine": source.database.engine, "range_id": manifest.range_id,
        "continuity": source.continuity,
        "collected_from": manifest.collected_from, "collected_until": manifest.collected_until
    });
    match captured.range {
        TelemetryRange::NativeInterval { interval } => {
            context["range_kind"] = json!("native-interval");
            context["interval_id"] = json!(interval.native_id);
            context["interval_from"] = json!(interval.from);
            context["interval_until"] = json!(interval.until);
        }
        TelemetryRange::TimeWindow { from, until } => {
            context["range_kind"] = json!("time-window");
            context["interval_from"] = json!(from);
            context["interval_until"] = json!(until);
        }
        TelemetryRange::Current => context["range_kind"] = json!("current"),
    }
    match &source.observation {
        SourceObservation::RetainedSamples(history) => context["observed_at"] = json!(history.observed_at),
        SourceObservation::RetainedEvents(history) => context["observed_at"] = json!(history.observed_at),
        SourceObservation::Activity(snapshot) => context["observed_at"] = json!(snapshot.at),
        SourceObservation::Snapshot(snapshot) => {
            context["dataset"] = json!(snapshot.dataset);
            context["observed_from"] = json!(snapshot.observed_from);
            context["observed_until"] = json!(snapshot.observed_until);
        }
        SourceObservation::QueryHistory(_) => (),
        SourceObservation::PagedSnapshot(_) => return Err(invalid()),
    }
    Ok(context)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
