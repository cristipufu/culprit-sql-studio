//! A file's native cumulative work and independent catalog gauges, without inferring a reset epoch.
use super::*;

/// A source-qualified logical file size. Unknown metadata is represented by `None`, separately from zero.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum FileSize {
    NotApplicable,
    Bytes { bytes: String },
}

impl FileSize {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::NotApplicable => true,
            Self::Bytes { bytes } => valid_counter(bytes),
        }
    }
}

/// The file's configured maximum, not database capacity, free space or permission to grow automatically.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum FileSizeLimit {
    NoGrowth,
    /// No fixed maximum is configured for this file. Storage, database and platform limits still apply.
    Unlimited,
    Bytes {
        bytes: String,
    },
}

impl FileSizeLimit {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::NoGrowth | Self::Unlimited => true,
            Self::Bytes { bytes } => valid_counter(bytes) && bytes.parse::<u64>().is_ok_and(|value| value > 0),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FileIoCounters {
    pub native_database_id: String,
    pub database: Option<String>,
    pub file_id: String,
    pub native_name: Option<String>,
    pub native_type: Option<String>,
    pub kind: crate::FileKind,
    pub context: crate::FileIoContext,
    pub scope: crate::FileScope,
    pub reset: crate::ResetMarker,
    pub reads: String,
    pub read_ms: Option<String>,
    /// Cumulative read latency introduced by native I/O resource governance, in milliseconds.
    pub queued_read_ms: Option<String>,
    pub read_bytes: Option<String>,
    pub writes: String,
    pub write_ms: Option<String>,
    /// Cumulative write latency introduced by native I/O resource governance, in milliseconds.
    pub queued_write_ms: Option<String>,
    pub write_bytes: Option<String>,
    /// Catalog gauge, independent of cumulative I/O and physical disk allocation.
    pub logical_size: Option<FileSize>,
    pub maximum_size: Option<FileSizeLimit>,
    pub size_on_disk_bytes: Option<String>,
    /// Native uptime ticks, when available. These are neither a UTC timestamp nor a reset generation.
    pub sample_uptime_ms: Option<String>,
}
impl FileIoCounters {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.native_database_id)
            && self.context.is_valid()
            && valid_key(&self.file_id)
            && [&self.database, &self.native_name, &self.native_type]
                .into_iter()
                .all(|value| value.as_deref().is_none_or(|value| !value.is_empty() && value.len() <= 2048))
            && valid_reset(&self.reset)
            && self.logical_size.as_ref().is_none_or(FileSize::is_valid)
            && self.maximum_size.as_ref().is_none_or(FileSizeLimit::is_valid)
            && [&self.reads, &self.writes].into_iter().all(|value| valid_counter(value))
            && [
                &self.read_ms,
                &self.queued_read_ms,
                &self.read_bytes,
                &self.write_ms,
                &self.queued_write_ms,
                &self.write_bytes,
                &self.size_on_disk_bytes,
                &self.sample_uptime_ms,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(valid_counter))
    }
}

#[cfg(test)]
mod tests;
