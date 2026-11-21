//! An explicitly selected native pool configuration point; no numeric ResourceGovernor identity is inferred.
use super::*;
use crate::read::{integer, optional_integer};
use culprit_core::{Error, ErrorCode, MeasurementScope, telemetry::*};

// Fixed user-resource-pool columns; database/workload-group limits are deliberately separate.
// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-user-db-resource-governor-azure-sql-database
const LAYOUT: &str = r#"SELECT TOP(7) /* culprit */ name,system_type_id FROM sys.all_columns
WHERE object_id=OBJECT_ID(N'sys.dm_user_db_resource_governance')
AND name IN(N'min_cpu',N'max_cpu',N'cap_cpu',N'min_memory',N'max_memory',N'pool_max_io')"#;
const COLUMNS: &str = ",DB_ID() AS selected_database_id,g.database_id,g.min_cpu,g.max_cpu,g.cap_cpu,g.min_memory,g.max_memory,g.pool_max_io";

// Optional elastic membership has narrower documented visibility than the governance DMV. Both catalog reads
// remain in the same original dispatch; no denied/hidden membership can become a known unpooled result.
// https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-database-service-objectives-azure-sql-database
fn query(extra_columns: &str) -> String {
    let assignment = format!("SELECT /* culprit */ @rows=COUNT(*),@pool=MAX(native.elastic_pool_name) FROM (SELECT TOP(2) s.elastic_pool_name {MEMBERSHIP_FROM}) native").replace('\'', "''");
    let projection = identity_projection(
        &format!(",g.database_name,@culprit_membership_visibility AS elastic_pool_visibility{extra_columns}"),
        "@culprit_pool_name",
        "",
    );
    format!(
        r#"DECLARE /* culprit */ @culprit_membership_rows int=0,@culprit_pool_name nvarchar(128)=NULL,
 @culprit_membership_visibility int=1;
BEGIN TRY
 IF OBJECT_ID(N'sys.database_service_objectives') IS NOT NULL
 BEGIN
  EXEC sys.sp_executesql N'{assignment}',N'@rows int OUTPUT,@pool nvarchar(128) OUTPUT',
   @rows=@culprit_membership_rows OUTPUT,@pool=@culprit_pool_name OUTPUT;
  IF @culprit_membership_rows=1 SET @culprit_membership_visibility=0;
 END;
END TRY
BEGIN CATCH
 IF ERROR_NUMBER() NOT IN(208,229,297,300) THROW;
 SET @culprit_membership_visibility=CASE WHEN ERROR_NUMBER() IN(229,297,300) THEN 2 ELSE 1 END;
END CATCH;
IF @culprit_membership_visibility<>0 SET @culprit_pool_name=NULL;
{projection}"#
    )
}
fn membership_error(row: &Rec) -> Result<Option<ErrorCode>> {
    match integer(row, "elastic_pool_visibility")? {
        0 => Ok(None),
        1 => Ok(Some(ErrorCode::Unsupported)),
        2 => Ok(Some(ErrorCode::PermissionDenied)),
        _ => Err(invalid_data()),
    }
}

/// One original native database route with optional, independently observed elastic membership.
#[derive(Clone, PartialEq)]
pub(crate) struct Context {
    pub(crate) target: CollectionTarget,
    database: crate::database_identity::Context,
    database_identity: culprit_core::DatabaseIdentity,
    membership: Membership,
    elastic_pool_error: Option<ErrorCode>,
    observed_at: String,
}
impl Context {
    pub(crate) async fn read(
        reader: &mut impl NativeRead,
        subject: &CollectionSubject,
        host: &str,
        port: u16,
    ) -> Result<Self> {
        let row = one(reader, &query("")).await?;
        let (membership, observed_at) = decode_membership(&row)?;
        let database = crate::database_identity::Context::from_governance(&row)?;
        let elastic_pool_error = membership_error(&row)?;
        let target = match subject {
            CollectionSubject::Database => database.identity(host, port).into(),
            CollectionSubject::Shared { resource }
                if elastic_pool_error.is_none()
                    && membership.pool.as_ref().is_some_and(|pool| &pool.identity == resource) =>
            {
                CollectionTarget::Shared(resource.clone())
            }
            _ => return Err(CollectionStop::SourceChanged.error()),
        };
        let database_identity = database.identity(host, port);
        let context = Self { target, database, database_identity, membership, elastic_pool_error, observed_at };
        if !context.settings_membership(None).matches_target(&context.target) {
            return Err(invalid_data());
        }
        Ok(context)
    }
    pub(crate) fn settings_membership(&self, point: Option<MssqlPoolSettingsPoint>) -> MssqlPoolSettingsMembership {
        // The database target's route is retained at initial acquisition; shared targets use that same route.
        MssqlPoolSettingsMembership {
            database: self.database_identity.clone(),
            elastic_pool: self.membership.pool.as_ref().map(|pool| pool.identity.clone()),
            elastic_pool_error: self.elastic_pool_error,
            member: self.membership.route.clone(),
            member_observed_at: self.observed_at.clone(),
            database_writable: self.database.writable == Some(true),
            point,
        }
    }
    pub(crate) async fn recheck(&self, reader: &mut impl NativeRead) -> Result<Self> {
        let row = one(reader, &query("")).await?;
        let observed_at = self.confirm_row(&row)?;
        let mut after = self.clone();
        after.observed_at = observed_at;
        Ok(after)
    }
    pub(crate) fn observed_at(&self) -> &str {
        &self.observed_at
    }
    pub(crate) fn confirmed_by(&self, after: &Self, observed_until: &str) -> bool {
        self.target == after.target
            && self.database == after.database
            && self.database_identity == after.database_identity
            && self.membership == after.membership
            && self.elastic_pool_error == after.elastic_pool_error
            && ObservationBounds::new(observed_until, &after.observed_at).is_some()
    }
    fn confirm_row(&self, row: &Rec) -> Result<String> {
        let (membership, at) = decode_membership(row)?;
        if membership != self.membership
            || membership_error(row)? != self.elastic_pool_error
            || crate::database_identity::Context::from_governance(row)? != self.database
            || ObservationBounds::new(&self.observed_at, &at).is_none()
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(at)
    }
}

pub(crate) struct Read {
    pub values: Vec<SettingObservation>,
    pub point: MssqlPoolSettingsPoint,
}

pub(crate) async fn read(reader: &mut impl NativeRead, owner: &Context, limit: usize) -> Result<Read> {
    if limit < MSSQL_POOL_SETTING_NAMES.len() {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .query(LAYOUT, vec![], MSSQL_POOL_SETTING_NAMES.len() + 1)
        .await
        .map_err(crate::read::ReadError::into_error)?;
    if !supported_layout(&rows)? {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let row = one(reader, &query(COLUMNS)).await?;
    let at = owner.confirm_row(&row)?;
    let database_id = integer(&row, "database_id")?;
    if database_id <= 0 || database_id != integer(&row, "selected_database_id")? {
        return Err(CollectionStop::SourceChanged.error());
    }
    let point =
        MssqlPoolSettingsPoint { database_id: database_id.try_into().map_err(|_| invalid_data())?, observed_at: at };
    let values = MSSQL_POOL_SETTING_NAMES.into_iter().map(|name| decode(&row, name)).collect::<Result<Vec<_>>>()?;
    Ok(Read { values, point })
}

fn supported_layout(rows: &[Rec]) -> Result<bool> {
    if rows.len() != MSSQL_POOL_SETTING_NAMES.len() {
        return Ok(false);
    }
    let mut names = std::collections::BTreeSet::new();
    for row in rows {
        let name = text(row, "name")?;
        if !MSSQL_POOL_SETTING_NAMES.contains(&name.as_str()) || !names.insert(name.clone()) {
            return Ok(false);
        }
        let expected = if matches!(name.as_str(), "min_cpu" | "max_cpu" | "cap_cpu") { 48 } else { 56 };
        if integer(row, "system_type_id")? != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode(row: &Rec, name: &str) -> Result<SettingObservation> {
    let value = optional_integer(row, name)?
        .map(|value| {
            let maximum = if name == "pool_max_io" { i64::from(i32::MAX) } else { 100 };
            (0..=maximum).contains(&value).then(|| value.to_string()).ok_or_else(invalid_data)
        })
        .transpose()?;
    let value = SettingObservation {
        source: MSSQL_POOL_SETTINGS_SOURCE.into(),
        namespace: MSSQL_POOL_SETTINGS_NAMESPACE.into(),
        name: name.into(),
        scope: MeasurementScope::Pool,
        nature: SettingNature::Configuration,
        value,
        value_in_use: None,
        value_for_secondary: None,
        unit: mssql_pool_setting_unit(name).map(str::to_owned),
        native_context: None,
        native_source: Some(MSSQL_POOL_SETTINGS_NAMESPACE.into()),
        pending_restart: None,
        native_is_default: None,
        default_value: None,
        default_source: SettingDefaultSource::Unknown,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
