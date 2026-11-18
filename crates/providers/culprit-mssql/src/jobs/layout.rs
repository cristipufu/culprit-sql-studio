//! One targeted metadata statement admits only fixed native objects/columns. NULL visibility stays unknown.
use super::*;
use std::collections::BTreeMap;

pub(super) const OBJECTS: [MssqlAgentObject; 6] = [
    MssqlAgentObject::Jobs,
    MssqlAgentObject::Steps,
    MssqlAgentObject::History,
    MssqlAgentObject::Activity,
    MssqlAgentObject::Sessions,
    MssqlAgentObject::Services,
];
fn fields(object: MssqlAgentObject) -> &'static [(&'static str, i64)] {
    match object {
        MssqlAgentObject::Jobs => &[
            ("job_id", 36),
            ("name", 231),
            ("enabled", 48),
            ("start_step_id", 56),
            ("version_number", 56),
            ("date_created", 61),
            ("date_modified", 61),
        ],
        MssqlAgentObject::Steps => {
            &[("job_id", 36), ("step_id", 56), ("step_name", 231), ("subsystem", 231), ("database_name", 231)]
        }
        MssqlAgentObject::History => &[
            ("instance_id", 56),
            ("job_id", 36),
            ("step_id", 56),
            ("step_name", 231),
            ("run_status", 56),
            ("run_date", 56),
            ("run_time", 56),
            ("run_duration", 56),
            ("sql_message_id", 56),
            ("sql_severity", 56),
            ("retries_attempted", 56),
            ("server", 231),
        ],
        MssqlAgentObject::Activity => &[
            ("session_id", 56),
            ("job_id", 36),
            ("run_requested_date", 61),
            ("run_requested_source", 231),
            ("queued_date", 61),
            ("start_execution_date", 61),
            ("stop_execution_date", 61),
            ("last_executed_step_id", 56),
            ("last_executed_step_date", 61),
            ("next_scheduled_run_date", 61),
        ],
        MssqlAgentObject::Sessions => &[("session_id", 56), ("agent_start_date", 61)],
        MssqlAgentObject::Services => &[("servicename", 231), ("status", 56), ("last_startup_time", 43)],
    }
}
pub(super) fn query(major: u32) -> String {
    let permission = if major >= 16 { "VIEW SERVER SECURITY STATE" } else { "VIEW SERVER STATE" };
    let rows = OBJECTS
        .iter()
        .enumerate()
        .flat_map(|(kind, object)| {
            fields(*object).iter().map(move |(column, _)| format!("({kind},N'{}',N'{column}')", object.native_name()))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"SELECT /* culprit */ wanted.kind,wanted.column_name,OBJECT_ID(wanted.object_name) AS object_id,
 IS_SRVROLEMEMBER(N'sysadmin') AS is_sysadmin,
 HAS_DBACCESS(N'msdb') AS msdb_access,
 HAS_PERMS_BY_NAME(wanted.object_name,N'OBJECT',N'SELECT') AS object_select,
 HAS_PERMS_BY_NAME(NULL,NULL,N'{permission}') AS service_visibility,
 CAST(COALESCE(msdb_columns.system_type_id,local_columns.system_type_id) AS int) AS native_type,
 CAST(COALESCE(msdb_columns.max_length,local_columns.max_length) AS int) AS native_length,
 CAST(COALESCE(msdb_columns.is_nullable,local_columns.is_nullable) AS int) AS native_nullable
 FROM (VALUES {rows}) wanted(kind,object_name,column_name)
 LEFT JOIN msdb.sys.columns msdb_columns ON wanted.kind<>5
  AND msdb_columns.object_id=OBJECT_ID(wanted.object_name) AND msdb_columns.name=wanted.column_name
 LEFT JOIN sys.all_columns local_columns ON wanted.kind=5
  AND local_columns.object_id=OBJECT_ID(wanted.object_name) AND local_columns.name=wanted.column_name
 ORDER BY wanted.kind,wanted.column_name"#
    )
}
pub(super) async fn read<R: NativeRead>(io: &mut R, edition: u32, major: u32) -> Result<MssqlAgentJobsMetadata> {
    let wanted = OBJECTS.iter().map(|object| fields(*object).len()).sum::<usize>();
    let rows = io.query(&query(major), vec![], wanted + 1).await.map_err(ReadError::into_error)?;
    if rows.len() != wanted {
        return Err(invalid_data());
    }
    let mut objects = BTreeMap::<usize, Vec<(&str, i64, i64, i64)>>::new();
    let mut permissions = BTreeMap::new();
    let mut identifiers = BTreeMap::new();
    let mut sysadmin = None;
    for row in &rows {
        let kind = usize::try_from(integer(row, "kind")?).map_err(|_| invalid_data())?;
        let object = *OBJECTS.get(kind).ok_or_else(invalid_data)?;
        let privileged = optional_integer(row, "is_sysadmin")?;
        if sysadmin.is_some_and(|value| value != privileged) {
            return Err(invalid_data());
        }
        sysadmin = Some(privileged);
        let access = access(row, object)?;
        let object_id = crate::read::optional_signed_integer(row, "object_id")?
            .map(i32::try_from)
            .transpose()
            .map_err(|_| invalid_data())?
            .map(|value| value.to_string());
        if identifiers.insert(kind, object_id.clone()).is_some_and(|previous| previous != object_id) {
            return Err(invalid_data());
        }
        if permissions.insert(kind, access).is_some_and(|previous| previous != access) {
            return Err(invalid_data());
        }
        let column = text(row, "column_name")?;
        let expected = fields(object).iter().find(|(name, _)| *name == column).ok_or_else(invalid_data)?;
        let actual = optional_integer(row, "native_type")?;
        let entry = objects.entry(kind).or_default();
        if entry.iter().any(|(name, _, _, _)| *name == column) {
            return Err(invalid_data());
        }
        // Keep only fixed expected column labels; the native types/nullability are fingerprinted below.
        entry.push((
            expected.0,
            actual.unwrap_or(-1),
            optional_integer(row, "native_length")?.unwrap_or(-1),
            optional_integer(row, "native_nullable")?.unwrap_or(-1),
        ));
    }
    let mut capabilities = Vec::new();
    for (kind, object) in OBJECTS.iter().copied().enumerate() {
        let actual = objects.get_mut(&kind).ok_or_else(invalid_data)?;
        actual.sort();
        let schema_matches = actual.len() == fields(object).len()
            && actual.iter().all(|(name, kind, _, nullable)| {
                fields(object).iter().any(|(expected, type_id)| expected == name && type_id == kind)
                    && matches!(nullable, 0 | 1)
            });
        let access = permissions[&kind];
        let state = if access == MssqlAgentAccess::Available && !schema_matches {
            MssqlAgentAccess::UnsupportedColumns
        } else {
            access
        };
        capabilities.push(MssqlAgentCapability {
            object,
            object_id: identifiers.remove(&kind).flatten(),
            access: state,
            columns_sha256: schema_matches
                .then(|| serde_json::to_vec(actual).map(|bytes| digest(&bytes)))
                .transpose()
                .map_err(|_| invalid_data())?,
        });
    }
    let metadata = MssqlAgentJobsMetadata {
        engine_edition: edition,
        major_version: major,
        availability: MssqlAgentAvailability::Supported,
        visibility: if sysadmin == Some(Some(1)) {
            MssqlAgentVisibility::Sysadmin
        } else if permissions.values().any(|access| *access == MssqlAgentAccess::Available) {
            MssqlAgentVisibility::ExplicitSelect
        } else {
            MssqlAgentVisibility::Unknown
        },
        capabilities,
        history_limit: MAX_HISTORY_ROWS,
        recheck: MssqlAgentMetadataRecheck::NotRead,
    };
    metadata.is_valid().then_some(metadata).ok_or_else(invalid_data)
}
fn access(row: &Rec, object: MssqlAgentObject) -> Result<MssqlAgentAccess> {
    Ok(match object {
        // The service DMV has its own versioned server permission. Its OBJECT SELECT result does not
        // establish that permission; a NULL relevant server permission still withholds availability.
        MssqlAgentObject::Services => access_from_permissions(&[optional_integer(row, "service_visibility")?]),
        _ => access_from_permissions(&[optional_integer(row, "object_select")?, optional_integer(row, "msdb_access")?]),
    })
}

fn access_from_permissions(permissions: &[Option<i64>]) -> MssqlAgentAccess {
    if permissions.contains(&Some(0)) {
        MssqlAgentAccess::PermissionDenied
    } else if permissions.iter().all(|value| *value == Some(1)) {
        MssqlAgentAccess::Available
    } else {
        MssqlAgentAccess::Unreported
    }
}
pub(super) fn unavailable(edition: u32, major: u32, access: MssqlAgentAccess) -> MssqlAgentJobsMetadata {
    MssqlAgentJobsMetadata {
        engine_edition: edition,
        major_version: major,
        availability: if access == MssqlAgentAccess::NotApplicable {
            MssqlAgentAvailability::NotApplicable
        } else {
            MssqlAgentAvailability::Supported
        },
        visibility: MssqlAgentVisibility::Unknown,
        capabilities: OBJECTS
            .into_iter()
            .map(|object| MssqlAgentCapability { object, access, columns_sha256: None, object_id: None })
            .collect(),
        history_limit: MAX_HISTORY_ROWS,
        recheck: MssqlAgentMetadataRecheck::NotRead,
    }
}
