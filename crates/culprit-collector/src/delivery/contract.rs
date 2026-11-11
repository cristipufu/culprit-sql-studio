//! Pipeline identity is persisted with backlog and cannot be changed by a configuration reload.
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest},
};
use serde::{Deserialize, Serialize};

pub const DELIVERY_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RANGE_RECORDS: usize = 100_000;
pub const MAX_BATCHES: usize = 256;
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RANGE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pipeline: String,
    shard: String,
    destination: String,
    schema_version: u32,
    export_format: u32,
    representation_version: u32,
    schema_digest: String,
}
impl Contract {
    pub fn new(pipeline: String, shard: String, destination: String, schema_digest: String) -> Result<Self> {
        let value = Self {
            pipeline,
            shard,
            destination,
            schema_digest,
            schema_version: DELIVERY_SCHEMA_VERSION,
            export_format: culprit_core::telemetry::EXPORT_FORMAT_VERSION,
            representation_version: culprit_core::telemetry::REPRESENTATION_VERSION,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        let identifier = |text: &str| {
            !text.is_empty()
                && text.len() <= 256
                && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
        };
        if !identifier(&self.pipeline)
            || !identifier(&self.shard)
            || !identifier(&self.destination)
            || !valid_digest(&self.schema_digest)
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        if self.schema_version != DELIVERY_SCHEMA_VERSION
            || self.export_format != culprit_core::telemetry::EXPORT_FORMAT_VERSION
            || self.representation_version != culprit_core::telemetry::REPRESENTATION_VERSION
        {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self).map_err(|_| DeliveryProblem::Integrity.error())?))
    }
    pub fn destination(&self) -> &str {
        &self.destination
    }
    pub fn shard(&self) -> &str {
        &self.shard
    }
    pub fn schema_digest(&self) -> &str {
        &self.schema_digest
    }
}
pub(crate) use culprit_core::telemetry::valid_digest;
pub(crate) fn bounded_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_./:".contains(&byte))
        && !value.split('/').any(|segment| segment == "..")
}
