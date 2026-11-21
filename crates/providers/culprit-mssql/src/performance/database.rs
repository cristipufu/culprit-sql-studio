//! One explicit selected population shares the native counter decoder and held source; names alone grant nothing.
use super::*;
use crate::{
    local_source::Context,
    read::{Parameter, integer, optional_integer, optional_text, text},
    sql_time::native_time,
    value::Rec,
};
use culprit_core::telemetry::*;
use std::collections::BTreeMap;

fn query(limit: usize) -> Result<String> {
    let profile = culprit_core::Engine::Mssql.spec().performance_counters().ok_or_else(invalid_data)?;
    let counters =
        profile.database_fields().map(|field| format!("N'{}'", field.native_name)).collect::<Vec<_>>().join(",");
    Ok(format!(
        r#"DECLARE /* culprit */ @counter_observed_at datetime2(7)=SYSUTCDATETIME(),
 @selected_id int=DB_ID(),@selected_name sysname=DB_NAME(),
 @updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability')),
 @catalog_visible int=HAS_PERMS_BY_NAME(NULL,NULL,N'ALTER ANY DATABASE'),@catalog_matches int;
SELECT /* culprit */ @catalog_matches=COUNT(*) FROM (
 SELECT TOP (2) d.database_id FROM sys.databases d
 WHERE SUBSTRING(CONVERT(varbinary(256),d.name),1,DATALENGTH(@selected_name))=CONVERT(varbinary(256),@selected_name)
) AS possible_names;
SELECT /* culprit */ @selected_id AS selected_database_id,@selected_name AS selected_database,
 @updateability AS updateability,@catalog_visible AS catalog_visible,@catalog_matches AS catalog_matches,
 CONVERT(varchar(40),CAST(d.create_date AS datetime2),126) AS database_created,
 CONVERT(varchar(36),r.database_guid) AS database_guid,CONVERT(varchar(36),r.recovery_fork_guid) AS recovery_fork_guid,
 CONVERT(varchar(36),f.file_guid) AS database_file_guid,
 CONVERT(varchar(40),@counter_observed_at,127)+N'Z' AS counter_observed_at,c.*
FROM sys.databases d
LEFT JOIN sys.database_recovery_status r ON r.database_id=d.database_id
LEFT JOIN sys.database_files f ON f.file_id=1
OUTER APPLY (
 SELECT TOP ({limit}) {COUNTER_COLUMNS}
 FROM sys.dm_os_performance_counters
 WHERE @catalog_visible=1 AND @catalog_matches=1
 AND RTRIM(object_name) LIKE N'%:Databases' AND counter_name IN({counters})
 AND CONVERT(varbinary(256),RTRIM(instance_name))=CONVERT(varbinary(256),@selected_name)
) c
WHERE d.database_id=@selected_id AND @selected_id=@P1 OPTION(MAXDOP 1,LOOP JOIN)"#
    ))
}

pub(crate) async fn read_current<R: NativeRead>(
    reader: &mut R,
    limit: usize,
    owner: &Context,
) -> Result<PerformanceRead> {
    require_owner(owner)?;
    require_connection(reader, owner)?;
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let maximum =
        culprit_core::Engine::Mssql.spec().performance_counters().ok_or_else(invalid_data)?.database_field_ids.len();
    let limit = limit.min(maximum);
    let rows = reader
        .query(&query(limit + 1)?, vec![Parameter::Integer(i64::from(owner.database_id))], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    require_connection(reader, owner)?;
    require_frame(&rows, owner)?;
    super::decode(&rows, limit, Population::Database(owner))
}
fn require_owner(owner: &Context) -> Result<()> {
    if !matches!(owner.activity.edition, 2 | 3 | 4) || owner.activity.major < 16 {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    if !owner.is_qualified()
        || owner.activity.writable != Some(true)
        || owner.connection_generation().is_none_or(|generation| generation == 0)
        || !database_performance_name_is_admissible(&owner.activity.database.database)
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(())
}
fn require_connection(reader: &impl NativeRead, owner: &Context) -> Result<()> {
    if reader.connection_generation() != owner.connection_generation() {
        Err(CollectionStop::SourceChanged.error())
    } else {
        Ok(())
    }
}
fn require_frame(rows: &[Rec], owner: &Context) -> Result<()> {
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut at = None;
    for row in rows {
        crate::cached_statements::acquisition_frame::require_role(row, owner.activity.writable)?;
        if integer(row, "selected_database_id")? != i64::from(owner.database_id)
            || text(row, "selected_database")? != owner.activity.database.database
            || native_time(&text(row, "database_created")?)? != owner.database_created
            || optional_text(row, "database_guid")? != owner.database_guid
            || optional_text(row, "recovery_fork_guid")? != owner.recovery_fork_guid
            || optional_text(row, "database_file_guid")? != owner.database_file_guid
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        match optional_integer(row, "catalog_visible")? {
            Some(1) => {}
            Some(0) => {
                return Err(culprit_core::Error::new(
                    culprit_core::ErrorCode::PermissionDenied,
                    culprit_core::t!("view.performance_counters.summary.unavailable"),
                ));
            }
            None => return Err(CollectionStop::SourceUnavailable.error()),
            _ => return Err(invalid_data()),
        }
        if integer(row, "catalog_matches").ok() != Some(1) {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let instant = canonical_time(&text(row, "counter_observed_at")?)?;
        if ObservationBounds::new(&owner.activity.at, &instant).is_none()
            || at.as_ref().is_some_and(|before| before != &instant)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        at = Some(instant);
        if optional_text(row, "native_object")?.is_none() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
    }
    Ok(())
}
pub(super) fn captured(rows: &[Rec], counters: Vec<NativeCounterSample>, owner: &Context) -> Result<PerformanceRead> {
    let sample = NativeCounterSet {
        identity: owner.native_identity()?,
        scope: MeasurementScope::Database,
        counters,
        ratios: vec![],
        ratio_gaps: vec![],
        properties: BTreeMap::from([
            (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), DATABASE_PERFORMANCE_COUNTER_PROFILE.into()),
            (DATABASE_PERFORMANCE_NAME_PROPERTY.into(), text(&rows[0], "selected_database")?),
            (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), canonical_time(&text(&rows[0], "counter_observed_at")?)?),
            (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), rows.len().to_string()),
            ("catalog_visibility".into(), "complete".into()),
            ("catalog_name_matches".into(), "1".into()),
        ]),
    };
    culprit_core::Engine::Mssql.spec().performance_counters().ok_or_else(invalid_data)?.validate(&sample)?;
    Ok(PerformanceRead {
        sample,
        native_rows: rows.len(),
        resource_governor: Vec::new(),
        resource_governor_complete: true,
    })
}

#[cfg(test)]
pub(crate) mod tests;
