//! Metadata-only, selected native database backup history. No media access, restore, backup or application work.
use crate::{
    deployment::NativeDeployment,
    read::{
        NativeRead, Parameter, ReadError, boolean, database_writable, integer, invalid_data, optional_boolean,
        optional_integer, optional_signed_integer, optional_text, text,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
mod authority;
pub(crate) use authority::Authority;
mod sql;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub scope: MssqlBackupHistoryScope,
    pub observed_at: String,
    pub layout_available: bool,
}
pub(crate) async fn metadata(reader: &mut impl NativeRead, authority: &Authority) -> Result<Metadata> {
    let rows = reader.query(&sql::metadata(authority.profile()), vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(invalid_data());
    };
    if integer(row, "engine_edition")? != authority.edition()
        || text(row, "major")?.parse::<u32>().map_err(|_| invalid_data())? != authority.major()
        || text(row, "database_name")? != authority.database().database
        || database_writable(row)? != Some(authority.writable()?)
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let deployment = NativeDeployment::decode(row)?;
    let hyperscale = if authority.profile() == MssqlBackupHistoryProfile::AzureSqlDatabase {
        if deployment.hyperscale() {
            Some(true)
        } else if deployment.conventional() {
            Some(false)
        } else {
            None
        }
    } else {
        None
    };
    Ok(Metadata {
        scope: authority.scope(integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?, hyperscale)?,
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        layout_available: boolean(row, "layout_available")?,
    })
}
impl Metadata {
    pub fn same_source(&self, after: &Self) -> bool {
        self.scope == after.scope
            && self.layout_available == after.layout_available
            && ObservationBounds::new(&self.observed_at, &after.observed_at).is_some()
    }
    pub fn unavailable(&self) -> Option<culprit_core::Error> {
        if self.scope.profile == MssqlBackupHistoryProfile::AzureSqlDatabase {
            match self.scope.hyperscale {
                Some(true) => {
                    return Some(culprit_core::Error::new(
                        culprit_core::ErrorCode::Unsupported,
                        culprit_core::t!("view.backup_history.limit.hyperscale"),
                    ));
                }
                None => return Some(CollectionStop::SourceUnavailable.error()),
                Some(false) => {}
            }
        }
        (!self.layout_available).then(|| CollectionStop::SourceUnavailable.error())
    }
}

pub(crate) struct Point {
    pub observed_at: String,
    pub record: MssqlBackupRecord,
}
impl Point {
    pub fn validate(&self, context: &NativeReadContext, scope: &MssqlBackupHistoryScope) -> Result<()> {
        if !scope.matches_context(context)
            || !self.record.is_valid_for(scope, context)
            || !ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
        {
            return Err(invalid_data());
        }
        Ok(())
    }
    pub fn bind(
        self,
        context: &NativeReadContext,
        scope: &MssqlBackupHistoryScope,
        position: u32,
    ) -> Result<OperationalObservation> {
        let point = MssqlBackupHistoryObservation {
            context: context.clone(),
            scope: scope.clone(),
            observed_at: self.observed_at,
            position,
            record: self.record,
        };
        Ok(OperationalObservation {
            source: MSSQL_BACKUP_HISTORY_SOURCE.into(),
            key: point.key()?,
            fact: OperationalFact::MssqlBackupHistory(Box::new(point)),
        })
    }
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    authority: &Authority,
    metadata: &Metadata,
    limit: u32,
) -> Result<Vec<Point>> {
    if limit == 0 || limit > MAX_MSSQL_BACKUP_HISTORY_ROWS || metadata.unavailable().is_some() {
        return Err(invalid_data());
    }
    let mut parameters =
        vec![Parameter::Integer(i64::from(limit) + 1), Parameter::Text(uuid_value(authority.database_guid()?)?)];
    if authority.profile() != MssqlBackupHistoryProfile::AzureSqlDatabase {
        parameters.push(Parameter::Text(authority.database().database));
    }
    let rows = reader
        .query(&sql::history(authority.profile()), parameters, limit as usize + 1)
        .await
        .map_err(ReadError::into_error)?;
    let points = rows.iter().map(|row| decode(row, authority, metadata)).collect::<Result<Vec<_>>>()?;
    if !backup_history_roster_is_unique(points.iter().map(|point| &point.record)) {
        return Err(invalid_data());
    }
    Ok(points)
}
fn decode(row: &Rec, authority: &Authority, metadata: &Metadata) -> Result<Point> {
    if integer(row, "selected_database_id")? != i64::from(metadata.scope.database_id)
        || text(row, "selected_database_name")? != authority.database().database
        || database_writable(row)? != Some(authority.writable()?)
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let record = match metadata.scope.profile {
        MssqlBackupHistoryProfile::AzureSqlDatabase => {
            let frame = crate::database_identity::Context::from_window(row)?;
            let original = authority.database();
            if frame.database != original.database
                || frame.writable != Some(authority.writable()?)
                || !frame
                    .incarnation
                    .as_ref()
                    .zip(original.incarnation.as_ref())
                    .is_some_and(|(current, original)| current.eq_ignore_ascii_case(original))
                || !frame.physical_incarnation().is_some_and(|id| {
                    metadata
                        .scope
                        .native_database
                        .qualifiers
                        .get("physical_database_guid")
                        .is_some_and(|original| id.eq_ignore_ascii_case(original))
                })
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            MssqlBackupRecord::AzureSqlDatabase(Box::new(MssqlAzureSqlBackup {
                backup_file_id: uuid(row, "backup_file_id")?,
                logical_database_id: uuid(row, "logical_database_id")?,
                physical_database_name: text(row, "physical_database_name")?,
                logical_server_name: text(row, "logical_server_name")?,
                logical_database_name: text(row, "logical_database_name")?,
                started_without_offset: crate::sql_time::native_time(&text(row, "started_without_offset")?)?,
                finished_without_offset: crate::sql_time::native_time(&text(row, "finished_without_offset")?)?,
                backup_type: backup_type(&text(row, "backup_type")?)?,
                in_retention: boolean(row, "in_retention")?,
            }))
        }
        MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb => {
            let fork = uuid(row, "selected_recovery_fork_guid")?;
            if !uuid(row, "selected_database_guid")?.eq_ignore_ascii_case(authority.database_guid()?)
                || !authority.recovery_fork().is_some_and(|original| fork.eq_ignore_ascii_case(original))
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            MssqlBackupRecord::Msdb(Box::new(MssqlMsdbBackup {
                backup_set_id: integer(row, "backup_set_id")?.try_into().map_err(|_| invalid_data())?,
                backup_set_uuid: uuid(row, "backup_set_uuid")?,
                database_guid: uuid(row, "database_guid")?,
                database_name: text(row, "database_name")?,
                family_guid: optional_uuid(row, "family_guid")?,
                first_recovery_fork_guid: optional_uuid(row, "first_recovery_fork_guid")?,
                last_recovery_fork_guid: optional_uuid(row, "last_recovery_fork_guid")?,
                started_without_offset: local_time(row, "started_without_offset")?,
                finished_without_offset: local_time(row, "finished_without_offset")?,
                time_zone_quarter_hours: optional_signed_integer(row, "time_zone")?
                    .map(i16::try_from)
                    .transpose()
                    .map_err(|_| invalid_data())?,
                backup_type: optional_text(row, "backup_type")?.map(|value| backup_type(&value)).transpose()?,
                size_bytes: optional_text(row, "size_bytes")?,
                compressed_size_bytes: optional_text(row, "compressed_size_bytes")?,
                initiator_user_name: optional_text(row, "user_name")?,
                server_name: optional_text(row, "server_name")?,
                machine_name: optional_text(row, "machine_name")?,
                software: MssqlBackupSoftware {
                    vendor_id: optional_signed_integer(row, "software_vendor_id")?
                        .map(i32::try_from)
                        .transpose()
                        .map_err(|_| invalid_data())?,
                    major: optional_integer(row, "software_major_version")?
                        .map(u8::try_from)
                        .transpose()
                        .map_err(|_| invalid_data())?,
                    minor: optional_integer(row, "software_minor_version")?
                        .map(u8::try_from)
                        .transpose()
                        .map_err(|_| invalid_data())?,
                    build: optional_signed_integer(row, "software_build_version")?
                        .map(i16::try_from)
                        .transpose()
                        .map_err(|_| invalid_data())?,
                },
                lsns: MssqlBackupLsns {
                    first: optional_text(row, "first_lsn")?,
                    last: optional_text(row, "last_lsn")?,
                    checkpoint: optional_text(row, "checkpoint_lsn")?,
                    database_backup: optional_text(row, "database_backup_lsn")?,
                    differential_base: optional_text(row, "differential_base_lsn")?,
                    fork_point: optional_text(row, "fork_point_lsn")?,
                },
                flags: MssqlBackupFlags {
                    copy_only: optional_boolean(row, "is_copy_only")?,
                    snapshot: optional_boolean(row, "is_snapshot")?,
                    checksums: optional_boolean(row, "has_backup_checksums")?,
                    damaged: optional_boolean(row, "is_damaged")?,
                    begins_log_chain: optional_boolean(row, "begins_log_chain")?,
                    incomplete_metadata: optional_boolean(row, "has_incomplete_metadata")?,
                },
            }))
        }
    };
    Ok(Point { observed_at: canonical_time(&text(row, "observed_at")?)?, record })
}
fn uuid_value(value: &str) -> Result<String> {
    let id = tiberius::Uuid::parse_str(value).map_err(|_| invalid_data())?;
    if id.is_nil() {
        return Err(invalid_data());
    }
    Ok(id.to_string())
}
fn uuid(row: &Rec, name: &str) -> Result<String> {
    uuid_value(&text(row, name)?)
}
fn optional_uuid(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|value| uuid_value(&value)).transpose()
}
fn local_time(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}
fn backup_type(value: &str) -> Result<MssqlBackupType> {
    Ok(match value {
        "D" => MssqlBackupType::Database,
        "I" => MssqlBackupType::DifferentialDatabase,
        "L" => MssqlBackupType::Log,
        "F" => MssqlBackupType::File,
        "G" => MssqlBackupType::DifferentialFile,
        "P" => MssqlBackupType::Partial,
        "Q" => MssqlBackupType::DifferentialPartial,
        _ => return Err(invalid_data()),
    })
}
#[cfg(test)]
pub(crate) mod tests;
