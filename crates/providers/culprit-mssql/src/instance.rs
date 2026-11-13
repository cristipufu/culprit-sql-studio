//! The native instance is independent of the member database used to reach it. Names identify the server's
//! own network namespace, not a global host UUID. Process/start ticks guard a capture; they are not resource IDs.
use crate::read::{NativeRead, integer, invalid_data, one, text};
use culprit_core::{Engine, Error, ErrorCode, Result, telemetry::*};

const MIN_SUPPORTED_MAJOR: u32 = 11;
const SUPPORTED_EDITIONS: &[i64] = &[2, 3, 4, 8];
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

pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<(Context, String)> {
    let row = one(reader, IDENTITY).await?;
    let edition = integer(&row, "edition")?;
    let major = text(&row, "major")?.parse::<u32>().map_err(|_| invalid_data())?;
    if !SUPPORTED_EDITIONS.contains(&edition) || major < MIN_SUPPORTED_MAJOR {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let identity = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new(text(&row, "machine_name")?).qualified("kind", "sql-server-network-machine"),
        native: NativeIdentity::new(text(&row, "server_name")?).qualified("kind", "sql-server-instance"),
    };
    let process_id = u64::try_from(integer(&row, "process_id")?).map_err(|_| invalid_data())?;
    if !identity.is_valid() || process_id == 0 {
        return Err(invalid_data());
    }
    let observed_at = canonical_time(&text(&row, "observed_at")?)?;
    let epoch = one(reader, EPOCH).await?;
    let started_at_ticks = u64::try_from(integer(&epoch, "started_at_ticks")?).map_err(|_| invalid_data())?;
    let now_ticks = u64::try_from(integer(&epoch, "ms_ticks")?).map_err(|_| invalid_data())?;
    if started_at_ticks > now_ticks {
        return Err(invalid_data());
    }
    Ok((Context { identity, major, edition, process_id, started_at_ticks }, observed_at))
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
