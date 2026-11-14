//! A file's native cumulative work, without inferring a reset epoch or an interval latency.
use super::*;

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
    pub read_bytes: Option<String>,
    pub writes: String,
    pub write_ms: Option<String>,
    pub write_bytes: Option<String>,
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
            && [&self.reads, &self.writes].into_iter().all(|value| valid_counter(value))
            && [
                &self.read_ms,
                &self.read_bytes,
                &self.write_ms,
                &self.write_bytes,
                &self.size_on_disk_bytes,
                &self.sample_uptime_ms,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(valid_counter))
    }
}
