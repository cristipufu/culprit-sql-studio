//! The native instance is independent of the member database used to reach it. Names identify the server's
//! own network namespace, not a global host UUID. Process/start ticks guard a capture; they are not resource IDs.
use crate::read::{NativeRead, integer, invalid_data, one, text};
use culprit_core::{Engine, Error, ErrorCode, Result, telemetry::*};

const MIN_SUPPORTED_MAJOR: u32 = 11;
pub(crate) const SUPPORTED_EDITIONS: &[i64] = &[2, 3, 4, 8];

/// Same-row observer witness for native singleton/counter reads. Callers supply only fixed native clock SQL;
/// `i` is the existing sys.dm_os_sys_info singleton, never another source or connection.
pub(crate) enum PointClock {
    CurrentUtc,
    PerformanceRead,
    ManagedInstanceRead,
}
pub(crate) fn point_columns(clock: PointClock) -> String {
    let clock = match clock {
        PointClock::CurrentUtc => "SYSUTCDATETIME()",
        PointClock::PerformanceRead => "@counter_observed_at",
        PointClock::ManagedInstanceRead => "@observed_at",
    };
    format!(
        r#"i.sqlserver_start_time_ms_ticks AS started_at_ticks,i.ms_ticks,
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CAST(SERVERPROPERTY('ProductMajorVersion') AS nvarchar(16)) AS major,
 CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name,
 CAST(SERVERPROPERTY('MachineName') AS nvarchar(128)) AS machine_name,
 CAST(SERVERPROPERTY('ProcessID') AS int) AS process_id,
 CONVERT(varchar(40),{clock},127)+'Z' AS observed_at"#
    )
}
pub(crate) fn supports_profile(edition: i64, major: u32) -> bool {
    SUPPORTED_EDITIONS.contains(&edition) && major >= MIN_SUPPORTED_MAJOR
}
const IDENTITY: &str = r#"SELECT /* culprit */
 CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CAST(SERVERPROPERTY('ProductMajorVersion') AS nvarchar(16)) AS major,
 CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name,
 CAST(SERVERPROPERTY('MachineName') AS nvarchar(128)) AS machine_name,
 CAST(SERVERPROPERTY('ProcessID') AS int) AS process_id,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS observed_at"#;
const EPOCH: &str = "SELECT /* culprit */ sqlserver_start_time_ms_ticks AS started_at_ticks,ms_ticks FROM sys.dm_os_sys_info OPTION(MAXDOP 1)";

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Context {
    pub identity: SharedResourceIdentity,
    pub major: u32,
    edition: i64,
    process_id: u64,
    started_at_ticks: u64,
}

/// Unsupported products still expose their observed platform; they never acquire invented instance facts.
pub(crate) struct Observation {
    pub edition: i64,
    pub major: u32,
    pub observed_at: String,
    native: Option<(SharedResourceIdentity, u64)>,
}
impl Observation {
    pub(crate) fn supports_instance(&self) -> bool {
        self.native.is_some()
    }
    pub(crate) fn identity(&self) -> Option<&SharedResourceIdentity> {
        self.native.as_ref().map(|(identity, _)| identity)
    }
    pub(crate) fn same_process(&self, other: &Self) -> bool {
        self.edition == other.edition
            && self.major == other.major
            && self.native.as_ref().zip(other.native.as_ref()).is_some_and(|(before, after)| before == after)
    }
    pub(crate) async fn context<R: NativeRead>(&self, reader: &mut R) -> Result<Option<Context>> {
        if self.native.is_none() {
            return Ok(None);
        }
        self.context_from_epoch(&one(reader, EPOCH).await?)
    }

    pub(crate) fn context_from_epoch(&self, epoch: &crate::value::Rec) -> Result<Option<Context>> {
        let Some((identity, process_id)) = &self.native else { return Ok(None) };
        let started_at_ticks = u64::try_from(integer(epoch, "started_at_ticks")?).map_err(|_| invalid_data())?;
        let now_ticks = u64::try_from(integer(epoch, "ms_ticks")?).map_err(|_| invalid_data())?;
        if started_at_ticks > now_ticks {
            return Err(invalid_data());
        }
        Ok(Some(Context {
            identity: identity.clone(),
            major: self.major,
            edition: self.edition,
            process_id: *process_id,
            started_at_ticks,
        }))
    }
}
impl Context {
    pub(crate) fn engine_edition(&self) -> i64 {
        self.edition
    }

    /// A local capture guard, not a resource ID or a global host/database identity.
    pub(crate) fn capture_epoch(&self) -> Result<String> {
        serde_json::to_vec(&(&self.identity, self.edition, self.major, self.process_id, self.started_at_ticks))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid_data())
    }
}

pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<(Context, String)> {
    let observed = observe(reader).await?;
    let context = observed
        .context(reader)
        .await?
        .ok_or_else(|| Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))?;
    Ok((context, observed.observed_at))
}

pub(crate) async fn observe<R: NativeRead>(reader: &mut R) -> Result<Observation> {
    let row = one(reader, IDENTITY).await?;
    observation(&row)
}

pub(crate) fn observation(row: &crate::value::Rec) -> Result<Observation> {
    let edition = integer(row, "edition")?;
    let major = text(row, "major")?.parse::<u32>().map_err(|_| invalid_data())?;
    let observed_at = canonical_time(&text(row, "observed_at")?)?;
    if !supports_profile(edition, major) {
        return Ok(Observation { edition, major, observed_at, native: None });
    }
    let identity = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new(text(row, "machine_name")?).qualified("kind", "sql-server-network-machine"),
        native: NativeIdentity::new(text(row, "server_name")?).qualified("kind", "sql-server-instance"),
    };
    let process_id = u64::try_from(integer(row, "process_id")?).map_err(|_| invalid_data())?;
    if !identity.is_valid() || process_id == 0 {
        return Err(invalid_data());
    }
    Ok(Observation { edition, major, observed_at, native: Some((identity, process_id)) })
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
