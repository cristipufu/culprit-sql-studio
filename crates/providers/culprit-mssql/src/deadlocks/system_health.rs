//! Nonflushing configured-file collection. Metadata witnesses describe the read owner at its boundaries,
//! never an older event's process/session generation or an uninterrupted retention horizon.
use super::native::{self, Event};
use crate::{
    instance,
    read::{NativeRead, ReadError, integer, invalid_data, optional_integer, optional_text, text},
};
use culprit_core::{
    Error, ErrorCode, Result, t,
    telemetry::{canonical_time, digest},
};

const MIN_NATIVE_MAJOR: u32 = 14;
const MAX_CONFIGURED_PATH_BYTES: i64 = 8_000;

// https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-xe-session-targets-transact-sql
// Persistent catalog configuration remains readable when the emitter is stopped. It does not read target_data.
// sys.dm_xe_session_targets would force target flushing; it is deliberately absent from this route.
const CONFIGURATION: &str = r#"SELECT TOP(2) /* culprit */
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at,
 c.event_session_id,target.target_id,target.package AS target_package,
 CONVERT(varchar(128),SQL_VARIANT_PROPERTY(f.value,'BaseType')) AS file_path_type,
 DATALENGTH(CONVERT(nvarchar(max),f.value)) AS file_path_bytes,
 CASE WHEN DATALENGTH(CONVERT(nvarchar(max),f.value))<=8000 THEN CONVERT(nvarchar(4000),f.value) END AS file_path
FROM sys.server_event_sessions c
JOIN sys.server_event_session_targets target ON target.event_session_id=c.event_session_id
JOIN sys.server_event_session_fields f ON f.event_session_id=c.event_session_id
 AND f.object_id=target.target_id AND f.name=N'filename'
WHERE c.name=N'system_health' AND target.name=N'event_file' AND target.package=N'package0'
 AND EXISTS(SELECT 1 FROM sys.server_event_session_events e
  WHERE e.event_session_id=c.event_session_id AND e.name=N'xml_deadlock_report' AND e.package=N'sqlserver')
OPTION(MAXDOP 1,LOOP JOIN)"#;

pub(crate) fn supported(instance: &instance::Context) -> bool {
    instance.major >= MIN_NATIVE_MAJOR
}

pub(crate) struct ConfiguredTarget {
    at: String,
    configuration_fingerprint: String,
    pattern: String,
}

impl ConfiguredTarget {
    pub(crate) async fn read<R: NativeRead>(io: &mut R) -> Result<Self> {
        let rows = io.query(CONFIGURATION, vec![], 2).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else { return Err(unavailable()) };
        let path_bytes = optional_integer(row, "file_path_bytes")?.ok_or_else(unavailable)?;
        if path_bytes < 0 {
            return Err(invalid_data());
        }
        if path_bytes > MAX_CONFIGURED_PATH_BYTES {
            return Err(culprit_core::telemetry::CollectionStop::ByteLimit.error());
        }
        let at = canonical_time(&text(row, "observed_at")?)?;
        let session_id = integer(row, "event_session_id")?;
        let target_id = integer(row, "target_id")?;
        if session_id < 0 || target_id < 0 || text(row, "target_package")? != "package0" {
            return Err(invalid_data());
        }
        let path_type = optional_text(row, "file_path_type")?.ok_or_else(unavailable)?;
        if !matches!(path_type.as_str(), "char" | "varchar" | "nchar" | "nvarchar") {
            return Err(unavailable());
        }
        let path = optional_text(row, "file_path")?.ok_or_else(unavailable)?;
        let pattern = configured_pattern(&path)?;
        let configuration_fingerprint = digest(
            &serde_json::to_vec(&("system-health-configured-file-v2", session_id, target_id, "package0", path))
                .map_err(|_| invalid_data())?,
        );
        Ok(Self { at, configuration_fingerprint, pattern })
    }

    pub(crate) fn observed_at(&self) -> &str {
        &self.at
    }

    pub(crate) fn same_configuration(&self, other: &Self) -> bool {
        self.configuration_fingerprint == other.configuration_fingerprint
    }

    pub(crate) fn confirmed_by(&self, current: &Self) -> bool {
        self.same_configuration(current)
            && culprit_core::telemetry::ObservationBounds::new(self.observed_at(), current.observed_at()).is_some()
    }

    pub(crate) fn capture_epoch(&self, instance: &instance::Context) -> Result<String> {
        serde_json::to_vec(&(instance.capture_epoch()?, &self.configuration_fingerprint))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid_data())
    }

    pub(crate) async fn events<R: NativeRead>(
        &self,
        io: &mut R,
        from: &str,
        until: &str,
        limit: usize,
        newest_first: bool,
    ) -> Result<Vec<Event>> {
        // TOP/row caps, byte limits and timeouts fence returned data/client retention. They do not bound
        // server file scanning or sorting across rollover files; the recipe remains Unqualified.
        native::read_file(io, &self.pattern, from, until, limit, newest_first).await
    }
}

fn configured_pattern(path: &str) -> Result<String> {
    let windows_drive = path.as_bytes().get(0..3).is_some_and(|prefix| {
        prefix[0].is_ascii_alphabetic() && prefix[1] == b':' && matches!(prefix[2], b'\\' | b'/')
    });
    let unix_absolute = path.starts_with('/');
    let unc_absolute = path.strip_prefix(r"\\").is_some_and(|tail| {
        let mut parts = tail.split('\\');
        parts.next().is_some_and(|server| !server.is_empty())
            && parts.next().is_some_and(|share| !share.is_empty())
            && parts.next().is_some_and(|file| !file.is_empty())
    });
    if !(windows_drive || unix_absolute || unc_absolute)
        || path.chars().any(char::is_control)
        || path.contains(['*', '?', '#'])
    {
        return Err(unavailable());
    }
    let (stem, extension) = path.rsplit_once('.').ok_or_else(unavailable)?;
    if !extension.eq_ignore_ascii_case("xel") || stem.ends_with(['/', '\\']) {
        return Err(unavailable());
    }
    Ok(format!("{stem}_*.{extension}"))
}

fn unavailable() -> Error {
    Error::new(ErrorCode::Unsupported, t!("provider.mssql.deadlocks.configured_file_unavailable"))
}

#[cfg(test)]
pub(crate) mod tests;
