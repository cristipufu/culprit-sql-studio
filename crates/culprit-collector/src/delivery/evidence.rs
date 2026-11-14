//! The delivery manifest carries exact row membership, independently from physical submission attempts.
use super::contract::*;
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

/// One canonical identity for a committed capture inside its exact delivery contract.
pub fn range_id_for(contract: &Contract, capture: &CaptureIdentity) -> Result<String> {
    serde_json::to_vec(&(contract, capture)).map(|bytes| digest(&bytes)).map_err(|_| DeliveryProblem::Integrity.error())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct RecordKey {
    pub batch_id: String,
    pub record_id: String,
    pub observation_id: String,
    pub owner_epoch: u64,
    pub sequence: u64,
    pub representation_version: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RowReceipt {
    pub key: RecordKey,
    pub payload_digest: String,
    pub native_key: NativeRecordIdentity,
    pub critical_fields_valid: bool,
}
impl RowReceipt {
    pub fn from_export(row: &ExportedRecord) -> Result<Self> {
        let revision = row.capture.revision.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        if row.format_version != EXPORT_FORMAT_VERSION
            || row.representation_version != REPRESENTATION_VERSION
            || !row.record.is_valid()
            || row.batch_id != batch_id(&row.capture.observation_id, row.record.stream())?
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        let positive = |value: &str| {
            value.parse::<u64>().ok().filter(|value| *value > 0).ok_or_else(|| DeliveryProblem::Integrity.error())
        };
        let record_id = row
            .record_id
            .clone()
            .filter(|value| valid_digest(value))
            .ok_or_else(|| DeliveryProblem::NotReady.error())?;
        Ok(Self {
            key: RecordKey {
                batch_id: row.batch_id.clone(),
                record_id,
                observation_id: row.capture.observation_id.clone(),
                owner_epoch: positive(&revision.owner_epoch)?,
                sequence: positive(&revision.sequence)?,
                representation_version: row.representation_version,
            },
            native_key: row.record.native_identity(),
            payload_digest: row.payload_sha256().map_err(|_| DeliveryProblem::Integrity.error())?,
            critical_fields_valid: true,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchManifest {
    pub batch_id: String,
    pub stream: TelemetryStream,
    pub object: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: Vec<RowReceipt>,
    pub delivery_required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamAdvance {
    pub stream_key: String,
    pub previous: Option<String>,
    pub next: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RangeManifest {
    pub contract: Contract,
    pub range_id: String,
    pub capture: CaptureIdentity,
    /// Digest of the original sealed CollectionManifest bytes, attested at preparation.
    pub capture_manifest_sha256: String,
    pub source: CollectionDescription,
    /// Collector clock readings, separate from native measurement and observation times.
    pub collected_from: String,
    pub collected_until: String,
    pub read_metrics: Option<ReadWorkMetrics>,
    pub batches: Vec<BatchManifest>,
    pub advances: Vec<StreamAdvance>,
}
impl RangeManifest {
    pub fn receipts_digest(&self) -> Result<String> {
        let rows = self
            .expected_receipts()
            .map(|receipt| (&receipt.key, receipt))
            .collect::<std::collections::BTreeMap<_, _>>();
        Ok(digest(
            &serde_json::to_vec(&rows.into_values().collect::<Vec<_>>())
                .map_err(|_| DeliveryProblem::Integrity.error())?,
        ))
    }
    pub fn validate(&self) -> Result<()> {
        self.contract.validate()?;
        if chrono::DateTime::parse_from_rfc3339(&self.collected_from).is_err()
            || chrono::DateTime::parse_from_rfc3339(&self.collected_until).is_err()
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        if self.read_metrics.as_ref().is_some_and(|metrics| !metrics.is_valid() || metrics.has_unconfirmed_work()) {
            return Err(DeliveryProblem::Integrity.error());
        }
        if !valid_digest(&self.range_id)
            || !valid_digest(&self.capture_manifest_sha256)
            || self.capture.revision.is_none()
            || self.batches.len() > MAX_BATCHES
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        let identity = range_id_for(&self.contract, &self.capture)?;
        if identity != self.range_id {
            return Err(DeliveryProblem::Integrity.error());
        }
        let revision = self.capture.revision.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let epoch = revision
            .owner_epoch
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| DeliveryProblem::Integrity.error())?;
        let sequence = revision
            .sequence
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| DeliveryProblem::Integrity.error())?;
        if !bounded_id(&self.capture.observation_id)
            || !bounded_id(&revision.outbox_id)
            || self.batches.len() != self.source.observation.collection().streams().len()
            || !self.source.observation.covers_request()
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        self.source.captured_range()?;
        let mut batches = BTreeSet::new();
        let mut rows = BTreeSet::new();
        let mut bytes = 0u64;
        for batch in &self.batches {
            bytes = bytes.checked_add(batch.bytes).ok_or_else(|| DeliveryProblem::Capacity.error())?;
            if batch.batch_id != batch_id(&self.capture.observation_id, batch.stream)?
                || !self.source.observation.collection().streams().contains(&batch.stream)
                || !valid_digest(&batch.batch_id)
                || !valid_digest(&batch.sha256)
                || !bounded_id(&batch.object)
                || !batches.insert(&batch.batch_id)
                || batch.delivery_required != (batch.stream.delivery_required())
            {
                return Err(DeliveryProblem::Integrity.error());
            }
            for row in &batch.rows {
                if row.key.batch_id != batch.batch_id
                    || row.key.observation_id != self.capture.observation_id
                    || row.key.owner_epoch != epoch
                    || row.key.sequence != sequence
                    || !valid_digest(&row.key.record_id)
                    || !valid_digest(&row.payload_digest)
                    || !row.critical_fields_valid
                    || row.key.representation_version == 0
                    || row.key.representation_version != REPRESENTATION_VERSION
                    || !row.native_key.is_valid()
                    || row.native_key.stream() != batch.stream
                    || !rows.insert(&row.key)
                {
                    return Err(DeliveryProblem::Integrity.error());
                }
            }
        }
        if bytes > MAX_RANGE_BYTES || rows.len() > MAX_RANGE_RECORDS {
            return Err(DeliveryProblem::Capacity.error());
        }
        let mut checkpoints = BTreeSet::new();
        for advance in &self.advances {
            if !bounded_id(&advance.stream_key)
                || !checkpoints.insert(&advance.stream_key)
                || advance.previous.as_ref().is_some_and(|cursor| cursor.len() > MAX_CURSOR_BYTES)
                || advance.next.is_empty()
                || advance.next.len() > MAX_CURSOR_BYTES
            {
                return Err(DeliveryProblem::Integrity.error());
            }
        }
        if serde_json::to_vec(self).map_err(|_| DeliveryProblem::Integrity.error())?.len() > MAX_MANIFEST_BYTES {
            return Err(DeliveryProblem::Capacity.error());
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(digest(&serde_json::to_vec(self).map_err(|_| DeliveryProblem::Integrity.error())?))
    }
    pub fn expected_receipts(&self) -> impl Iterator<Item = &RowReceipt> {
        self.batches.iter().filter(|batch| batch.delivery_required).flat_map(|batch| batch.rows.iter())
    }
}
/// Created only from a ready outbox capture which has passed the journal's artifact audit.
pub struct PreparedRange {
    pub(crate) manifest: Arc<RangeManifest>,
    pub(crate) directory: PathBuf,
}
impl PreparedRange {
    pub fn manifest(&self) -> &RangeManifest {
        &self.manifest
    }
    pub(crate) fn from_validated(
        directory: &Path,
        manifest: &CollectionManifest,
        contract: Contract,
        advances: Vec<StreamAdvance>,
    ) -> Result<Self> {
        if manifest.status != CollectionStatus::Complete || !manifest.references.complete {
            return Err(DeliveryProblem::NotReady.error());
        }
        let mut native_manifest_bytes = Vec::new();
        crate::durable_fs::regular_file(&directory.join("manifest.json"))
            .map_err(|_| DeliveryProblem::Storage.error())?
            .take(128 * 1024 + 1)
            .read_to_end(&mut native_manifest_bytes)
            .map_err(|_| DeliveryProblem::Storage.error())?;
        if native_manifest_bytes.len() > 128 * 1024
            || serde_json::from_slice::<serde_json::Value>(&native_manifest_bytes)
                .map_err(|_| DeliveryProblem::Integrity.error())?
                != serde_json::to_value(manifest).map_err(|_| DeliveryProblem::Integrity.error())?
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        let mut batches = Vec::new();
        let mut total_rows = 0usize;
        for file in &manifest.files {
            let mut input = BufReader::new(
                crate::durable_fs::regular_file(&directory.join(&file.file))
                    .map_err(|_| DeliveryProblem::Storage.error())?
                    .take(u64::from(file.bytes) + 1),
            );
            let mut rows = Vec::new();
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = (&mut input)
                    .take(MAX_RECORD_BYTES as u64 + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(|_| DeliveryProblem::Storage.error())?;
                if read == 0 {
                    break;
                }
                if line.len() > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
                    return Err(DeliveryProblem::Integrity.error());
                }
                let row: ExportedRecord =
                    serde_json::from_slice(&line).map_err(|_| DeliveryProblem::Integrity.error())?;
                if row.batch_id != file.batch_id || row.capture != manifest.capture || !row.record.is_valid() {
                    return Err(DeliveryProblem::Integrity.error());
                }
                rows.push(RowReceipt::from_export(&row)?);
                total_rows += 1;
                if total_rows > MAX_RANGE_RECORDS {
                    return Err(DeliveryProblem::Capacity.error());
                }
            }
            if rows.len() != file.rows as usize {
                return Err(DeliveryProblem::Integrity.error());
            }
            let sha = file.sha256.clone().ok_or_else(|| DeliveryProblem::Integrity.error())?;
            batches.push(BatchManifest {
                batch_id: file.batch_id.clone(),
                stream: file.stream,
                object: format!("payloads/{sha}.ndjson"),
                sha256: sha,
                bytes: u64::from(file.bytes),
                rows,
                delivery_required: file.stream.delivery_required(),
            });
        }
        let range_id = range_id_for(&contract, &manifest.capture)?;
        let value = RangeManifest {
            contract,
            range_id,
            capture: manifest.capture.clone(),
            capture_manifest_sha256: digest(&native_manifest_bytes),
            source: manifest.description.clone().ok_or_else(|| DeliveryProblem::Integrity.error())?,
            collected_from: manifest.started_at.clone(),
            collected_until: manifest.finished_at.clone().ok_or_else(|| DeliveryProblem::NotReady.error())?,
            read_metrics: manifest.read_metrics.clone(),
            batches,
            advances,
        };
        value.validate()?;
        Ok(Self { manifest: Arc::new(value), directory: directory.into() })
    }
    pub(crate) fn batch_path(&self, batch: &BatchManifest) -> PathBuf {
        self.directory.join(batch.stream.file_name())
    }
}
