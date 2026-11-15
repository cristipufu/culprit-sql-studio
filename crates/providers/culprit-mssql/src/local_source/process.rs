//! Process startup is an opaque local source epoch. It is neither a UTC timestamp nor a stable compute ID.
use crate::{
    read::{boolean, invalid_data, optional_counter, optional_text},
    value::Rec,
};
use culprit_core::{Result, telemetry::digest};
use serde::Serialize;

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct ProcessEpoch {
    process_id: String,
    started_ticks: String,
    started_without_offset: String,
}

impl ProcessEpoch {
    pub(super) fn decode(row: &Rec) -> Result<Option<Self>> {
        let process_id = optional_counter(row, "local_process_id")?;
        let started_ticks = optional_counter(row, "local_started_ticks")?;
        let now_ticks = optional_counter(row, "local_now_ticks")?;
        let started = optional_text(row, "local_started_without_offset")?;
        let visible = boolean(row, "local_epoch_visible")?;
        if !visible {
            if started_ticks.is_some() || now_ticks.is_some() || started.is_some() {
                return Err(invalid_data());
            }
            return Ok(None);
        }
        let (Some(process_id), Some(started_ticks), Some(now_ticks), Some(started)) =
            (process_id, started_ticks, now_ticks, started)
        else {
            return Ok(None);
        };
        let ticks = |value: &str| value.parse::<u64>().map_err(|_| invalid_data());
        if ticks(&process_id)? == 0 || ticks(&started_ticks)? > ticks(&now_ticks)? {
            return Err(invalid_data());
        }
        Ok(Some(Self { process_id, started_ticks, started_without_offset: crate::sql_time::native_time(&started)? }))
    }

    pub(super) fn key(&self) -> Result<String> {
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| invalid_data())
    }
}

pub(super) const COLUMNS: &str = "@local_process_id AS local_process_id,@local_started_ticks AS local_started_ticks,@local_now_ticks AS local_now_ticks,@local_started_without_offset AS local_started_without_offset,@local_epoch_visible AS local_epoch_visible";

/// The Azure state-reader membership proof is conservative in user databases. NULL/0, contained principals or
/// an unverified administrator never acquire an epoch. No virtual-master/SLO probe or instrumentation is used.
/// The DMV has one native row; permission failures withhold only this authority, preserving the actor baseline.
/// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-sys-info-transact-sql
/// https://learn.microsoft.com/azure/azure-sql/database/security-server-roles
pub(super) const AZURE_READ: &str = r#"
DECLARE /* culprit */ @local_process_id bigint=TRY_CONVERT(bigint,SERVERPROPERTY('ProcessID')),
 @local_started_ticks bigint=NULL,@local_now_ticks bigint=NULL,@local_started_without_offset varchar(40)=NULL,
 @local_epoch_visible bit=0;
IF DB_NAME()<>'master' AND IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1
 AND HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1
BEGIN
 BEGIN TRY
  SELECT /* culprit */ @local_started_ticks=sqlserver_start_time_ms_ticks,@local_now_ticks=ms_ticks,
   @local_started_without_offset=CONVERT(varchar(40),CAST(sqlserver_start_time AS datetime2),126)
  FROM sys.dm_os_sys_info OPTION(MAXDOP 1);
  SET @local_epoch_visible=1;
 END TRY
 BEGIN CATCH
  IF ERROR_NUMBER() NOT IN(229,297,300) THROW;
  SET @local_started_ticks=NULL;
  SET @local_now_ticks=NULL;
  SET @local_started_without_offset=NULL;
 END CATCH
END;
"#;

pub(super) const UNAVAILABLE_COLUMNS: &str = "CAST(NULL AS bigint) AS local_process_id,CAST(NULL AS bigint) AS local_started_ticks,CAST(NULL AS bigint) AS local_now_ticks,CAST(NULL AS varchar(40)) AS local_started_without_offset,CAST(0 AS bit) AS local_epoch_visible";
