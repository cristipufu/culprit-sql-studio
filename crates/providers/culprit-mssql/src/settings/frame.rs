//! One catalog result carries its batch-start authority. Native control fields never become settings.
use crate::{
    local_source::Context,
    read::{NativeRead, ReadError, database_writable_field, integer, invalid_data, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::CollectionStop};

const MARKER: &str = "culprit_settings_frame";
const DATABASE_ID: &str = "culprit_settings_database_id";
const DATABASE_NAME: &str = "culprit_settings_database_name";
const UPDATEABILITY: &str = "culprit_settings_updateability";
const CONTROL_COLUMNS: [&str; 4] = [MARKER, DATABASE_ID, DATABASE_NAME, UPDATEABILITY];
pub(super) const COLUMNS: &str = "CAST(1 AS int) AS culprit_settings_frame,\
 @culprit_settings_database_id AS culprit_settings_database_id,\
 @culprit_settings_database_name AS culprit_settings_database_name,\
 @culprit_settings_updateability AS culprit_settings_updateability,";
pub(super) const EXECUTION_PARAMETERS: &str = ",N'@culprit_settings_database_id int,\
 @culprit_settings_database_name sysname,@culprit_settings_updateability nvarchar(60)',\
 @culprit_settings_database_id=@culprit_settings_database_id,\
 @culprit_settings_database_name=@culprit_settings_database_name,\
 @culprit_settings_updateability=@culprit_settings_updateability";
const FRAME: &str = "DECLARE /* culprit */ @culprit_settings_database_id int=DB_ID(),\
 @culprit_settings_database_name sysname=DB_NAME(),\
 @culprit_settings_updateability nvarchar(60)=CONVERT(nvarchar(60),DATABASEPROPERTYEX(DB_NAME(),N'Updateability'));";

pub(super) async fn read<R: NativeRead>(reader: &mut R, sql: &str, limit: usize, owner: &Context) -> Result<Vec<Rec>> {
    if !owner.is_qualified() || owner.connection_generation().is_none() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    require_connection(reader, owner)?;
    let batch = format!("{FRAME}\n{sql}");
    let mut rows = reader.query(&batch, vec![], limit).await.map_err(ReadError::into_error)?;
    require_connection(reader, owner)?;
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    for row in &mut rows {
        require_frame(row, owner)?;
        row.retain_columns(|name| !CONTROL_COLUMNS.contains(&name));
    }
    Ok(rows)
}

fn require_frame(row: &Rec, owner: &Context) -> Result<()> {
    if CONTROL_COLUMNS.iter().any(|name| row.columns().filter(|(column, _)| column == name).count() != 1) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    if integer(row, MARKER)? != 1 {
        return Err(invalid_data());
    }
    crate::cached_statements::acquisition_frame::require_writable(
        database_writable_field(row, UPDATEABILITY)?,
        owner.activity.writable,
    )?;
    if u32::try_from(integer(row, DATABASE_ID)?).ok() != Some(owner.database_id)
        || text(row, DATABASE_NAME)? != owner.activity.database.database
    {
        return Err(CollectionStop::SourceChanged.error());
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

#[cfg(test)]
mod tests;
