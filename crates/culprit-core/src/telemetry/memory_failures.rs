//! Retained native memory-failure rows are observations, not an exactly-once event history or query attribution.
use super::*;
use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::de::IgnoredAny;
use std::collections::BTreeSet;

pub use crate::operations::MemoryFailureSnapshot;

pub const MAX_MEMORY_FAILURE_OBSERVATIONS: usize = 4096;
pub const MAX_MEMORY_FAILURE_JSON_UTF16: usize = 4000;
const MAX_NATIVE_DESCRIPTION_UTF16: usize = 30;

/// Parsing qualification does not change or discard the original bounded native document.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MemoryFailureJsonQualification {
    Parsed,
    /// Malformed input or a representation outside the bounded parser's supported depth/number range.
    Unparsed,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MemoryFailureJsonDocument {
    pub raw: String,
    pub qualification: MemoryFailureJsonQualification,
}

impl MemoryFailureJsonDocument {
    pub fn from_raw(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        if !bounded_utf16(&raw, MAX_MEMORY_FAILURE_JSON_UTF16) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self { qualification: json_qualification(&raw), raw })
    }

    pub fn is_valid(&self) -> bool {
        bounded_utf16(&self.raw, MAX_MEMORY_FAILURE_JSON_UTF16) && self.qualification == json_qualification(&self.raw)
    }
}

fn bounded_utf16(value: &str, limit: usize) -> bool {
    // Reject grossly oversized input before walking it. A UTF-16 unit needs at most three UTF-8 bytes.
    value.len() <= limit * 3 && value.encode_utf16().take(limit + 1).count() <= limit
}

fn json_qualification(raw: &str) -> MemoryFailureJsonQualification {
    let mut parser = serde_json::Deserializer::from_str(raw);
    if IgnoredAny::deserialize(&mut parser).is_ok() && parser.end().is_ok() {
        MemoryFailureJsonQualification::Parsed
    } else {
        MemoryFailureJsonQualification::Unparsed
    }
}

/// One row present in `sys.dm_os_out_of_memory_events` at capture time. Native causes are reported classifications;
/// the internal diagnostic factor is separate and establishes no corrective action or allocation owner.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlOutOfMemoryObservation {
    /// Zero-based position in this frozen source read. It is not a native occurrence ID across captures.
    pub position: u32,
    /// Native SQL `datetime`; its offset is not established. Never compare it with UTC by appending an offset.
    pub event_time_without_offset: String,
    pub oom_cause: u8,
    pub oom_cause_desc: String,
    pub oom_factor: u8,
    pub oom_factor_desc: String,
    /// Native signed integer MB values. Their byte basis and negative diagnostic sentinels are not reinterpreted.
    pub available_physical_memory_mb: i32,
    pub process_memory_usage_mb: i32,
    pub non_sos_memory_usage_mb: i32,
    pub committed_memory_target_mb: i32,
    pub committed_memory_mb: i32,
    pub allocation_potential_memory_mb: i32,
    pub initial_job_object_memory_limit_mb: Option<i32>,
    pub current_job_object_memory_limit_mb: Option<i32>,
    pub possible_non_sos_leaked_memory_mb: Option<i32>,
    /// SQL NULL, a JSON `null` document and an unparsed document remain distinct.
    pub oom_resource_pools: Option<MemoryFailureJsonDocument>,
    pub top_memory_clerks: Option<MemoryFailureJsonDocument>,
    pub top_resource_pools: Option<MemoryFailureJsonDocument>,
    pub possible_leaked_memory_clerks: Option<MemoryFailureJsonDocument>,
}

impl MssqlOutOfMemoryObservation {
    /// The outer operational-record identity supplies the capture and subject. Identical native rows at different
    /// positions remain distinct; a position or content hash never identifies an occurrence across captures.
    pub fn key(&self) -> Result<String> {
        if self.position as usize >= MAX_MEMORY_FAILURE_OBSERVATIONS {
            return Err(CollectionStop::InvalidPage.error());
        }
        let bytes = serde_json::to_vec(&("mssql-memory-failure-position-v1", self.position))
            .map_err(|_| CollectionStop::InvalidPage.error())?;
        Ok(digest(&bytes))
    }

    pub fn is_valid(&self) -> bool {
        (self.position as usize) < MAX_MEMORY_FAILURE_OBSERVATIONS
            && native_datetime(&self.event_time_without_offset)
            && [&self.oom_cause_desc, &self.oom_factor_desc]
                .into_iter()
                .all(|value| bounded_utf16(value, MAX_NATIVE_DESCRIPTION_UTF16))
            && [
                &self.oom_resource_pools,
                &self.top_memory_clerks,
                &self.top_resource_pools,
                &self.possible_leaked_memory_clerks,
            ]
            .into_iter()
            .flatten()
            .all(MemoryFailureJsonDocument::is_valid)
    }
}

fn native_datetime(value: &str) -> bool {
    if !(19..=23).contains(&value.len()) || !value.is_ascii() {
        return false;
    }
    let Ok(time) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") else { return false };
    (1753..=9999).contains(&time.year()) && time.nanosecond() < 1_000_000_000
}

impl MemoryFailureSnapshot {
    pub fn is_valid(&self) -> bool {
        let resource_matches = match self.scope {
            crate::MeasurementScope::Server => SharedResourceKind::Instance,
            crate::MeasurementScope::Pool => SharedResourceKind::Pool,
            _ => return false,
        };
        let mut positions = BTreeSet::new();
        valid_key(&self.source)
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && matches!(
                (DateTime::parse_from_rfc3339(&self.observed_from), DateTime::parse_from_rfc3339(&self.observed_until)),
                (Ok(from), Ok(until)) if from <= until
            )
            && match &self.resource {
                Some(resource) => {
                    resource.is_valid() && resource.engine == crate::Engine::Mssql && resource.kind == resource_matches
                }
                None => self.scope == crate::MeasurementScope::Server,
            }
            && self.observations.len() <= MAX_MEMORY_FAILURE_OBSERVATIONS
            && self.observations.iter().all(|row| row.is_valid() && positions.insert(row.position))
    }
}

#[cfg(test)]
mod tests;
