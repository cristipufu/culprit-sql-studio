use super::*;
use crate::query_store::{catalog_sql, decode, replicas};
use serde::Serialize;

// The same current layout SELECT is embedded unchanged, so this experiment observes the real native profile.
// DBCC only reads the explicit flag status. It never enables the SQL2022 limited preview.
pub(super) fn metadata_sql() -> String {
    format!(
        r#"SELECT /* culprit */ 1 AS culprit_profile_marker,l.*,
 CONVERT(varchar(40),SYSUTCDATETIME(),127)+'Z' AS profile_observed_at,
 CAST(SERVERPROPERTY('ProductMajorVersion') AS int) AS profile_major,
 CAST(SERVERPROPERTY('IsHadrEnabled') AS int) AS profile_hadr_enabled,
 DB_ID() AS profile_database_id,CONVERT(nvarchar(128),DB_NAME()) AS profile_database,
 o.desired_state AS profile_desired_state,o.actual_state AS profile_actual_state,
 o.readonly_reason AS profile_readonly_reason,
 HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE PERFORMANCE STATE') AS profile_performance_permission
FROM ({}) l CROSS JOIN sys.database_query_store_options o"#,
        runtime::LAYOUT,
    )
}

pub(super) const TRACE_STATUS_SQL: &str = "DBCC /* culprit */ TRACESTATUS(12606,-1) WITH NO_INFOMSGS";

pub(super) fn runtime_sql() -> String {
    format!(
        r#"SELECT TOP(3) /* culprit */ {},r.runtime_stats_id,r.plan_id,r.runtime_stats_interval_id,
 r.replica_group_id,r.count_executions
FROM sys.query_store_query q
JOIN sys.query_store_plan p ON p.query_id=q.query_id
JOIN sys.query_store_runtime_stats r ON r.plan_id=p.plan_id
WHERE q.query_id=@P1
ORDER BY r.runtime_stats_id,r.plan_id OPTION(MAXDOP 1)"#,
        catalog_sql::query_identity_columns!("q.query_id")
    )
}

#[derive(Serialize, PartialEq, Eq)]
struct ProfileFacts {
    native_major: Option<i64>,
    hadr_enabled: Option<bool>,
    database_id: Option<String>,
    database: Option<String>,
    desired_state: Option<i64>,
    actual_state: Option<i64>,
    readonly_reason: Option<String>,
    performance_permission: Option<bool>,
}
impl ProfileFacts {
    fn is_complete(&self) -> bool {
        self.native_major.is_some_and(|major| major > 0)
            && self.hadr_enabled.is_some()
            && self.database_id.as_ref().is_some_and(|id| id.parse::<u32>().is_ok_and(|id| id > 0))
            && self.database.is_some()
            && self.desired_state.is_some_and(|state| matches!(state, 0 | 1 | 2 | 4))
            && self.actual_state.is_some_and(|state| (0..=4).contains(&state))
            && self.readonly_reason.is_some()
            && self.performance_permission.is_some()
    }
}

#[derive(Serialize, PartialEq, Eq)]
struct TraceStatus {
    trace_flag: i64,
    status: bool,
    global: bool,
    session: bool,
}
impl TraceStatus {
    fn is_consistent(&self) -> bool {
        self.status == (self.global || self.session)
    }
}

#[derive(Serialize)]
struct ComponentError {
    code: culprit_core::ErrorCode,
    detail: Option<culprit_core::ErrorDetail>,
}
impl From<culprit_core::Error> for ComponentError {
    fn from(error: culprit_core::Error) -> Self {
        Self { code: error.code, detail: error.detail }
    }
}

#[derive(Serialize)]
pub(super) struct NativeProfilePoint {
    metadata_rows: usize,
    observed_at: Option<String>,
    facts: ProfileFacts,
    layout_fields: Option<serde_json::Value>,
    layout_error: Option<ComponentError>,
    trace_rows: usize,
    trace: Option<TraceStatus>,
    trace_error: Option<ComponentError>,
    catalog_rows: usize,
    catalog_limit_reached: bool,
    catalog: Option<culprit_core::RuntimePopulationCatalog>,
    catalog_error: Option<ComponentError>,
    #[serde(skip_serializing)]
    native_layout: Option<NativeLayout>,
    #[serde(skip_serializing)]
    native_catalog: Option<replicas::Catalog>,
}
impl NativeProfilePoint {
    pub(super) fn decode(rows: &[Rec]) -> Result<Self> {
        let mut metadata = Vec::new();
        let mut trace = Vec::new();
        let mut catalog = Vec::new();
        for row in rows {
            if has(row, "culprit_profile_marker") {
                metadata.push(row);
            } else if has(row, "traceflag") {
                trace.push(row);
            } else if has(row, "replica_group_id") && has(row, "role_type") {
                catalog.push(row.clone());
            } else {
                return Err(invalid_data());
            }
        }
        // Never choose a representative row when the supposedly singular metadata or flag result is ambiguous.
        let row = if metadata.len() == 1 { Some(metadata[0]) } else { None };
        let observed_at = field_text(row, "profile_observed_at")?.map(|at| canonical_time(&at)).transpose()?;
        let facts = ProfileFacts {
            native_major: field_integer(row, "profile_major")?,
            hadr_enabled: field_boolean(row, "profile_hadr_enabled")?,
            database_id: field_integer(row, "profile_database_id")?.map(|value| value.to_string()),
            database: field_text(row, "profile_database")?,
            desired_state: field_integer(row, "profile_desired_state")?,
            actual_state: field_integer(row, "profile_actual_state")?,
            readonly_reason: field_integer(row, "profile_readonly_reason")?.map(|value| value.to_string()),
            performance_permission: field_boolean(row, "profile_performance_permission")?,
        };
        let layout = row.map(NativeLayout::decode);
        let (native_layout, layout_error) = component(layout);
        let layout_fields = native_layout.map(|layout| {
            json!({
                "engine_edition":layout.edition,"replica_group_column_present":layout.partitioned,
                "tempdb_column_present":layout.tempdb,"log_bytes_column_present":layout.log_bytes,
                "waits_supported":layout.waits,"query_store_mode":format!("{:?}",layout.mode),
            })
        });
        let trace_result = if trace.len() == 1 { Some(decode_trace(trace[0])) } else { None };
        let (trace_value, trace_error) = component(trace_result);
        let catalog_result = if catalog.is_empty() { None } else { Some(replicas::Catalog::decode(&catalog)) };
        let (native_catalog, catalog_error) = component(catalog_result);
        let public_catalog = native_catalog.as_ref().map(replicas::Catalog::population_catalog).transpose()?;
        Ok(Self {
            metadata_rows: metadata.len(),
            observed_at,
            facts,
            layout_fields,
            layout_error,
            trace_rows: trace.len(),
            trace: trace_value,
            trace_error,
            catalog_rows: catalog.len(),
            catalog_limit_reached: catalog.len() > replicas::MAX_REPLICA_GROUP_ROWS,
            catalog: public_catalog,
            catalog_error,
            native_layout,
            native_catalog,
        })
    }
    pub(super) fn require_database(&self, expected: &str) -> Result<()> {
        if self.facts.database.as_deref().is_some_and(|database| database != expected) {
            Err(CollectionStop::SourceChanged.error())
        } else {
            Ok(())
        }
    }
    pub(super) fn same_configuration(&self, other: &Self) -> Option<bool> {
        if !self.facts.is_complete()
            || !other.facts.is_complete()
            || self.observed_at.is_none()
            || other.observed_at.is_none()
        {
            return None;
        }
        let before_clock = chrono::DateTime::parse_from_rfc3339(self.observed_at.as_deref()?).ok()?;
        let after_clock = chrono::DateTime::parse_from_rfc3339(other.observed_at.as_deref()?).ok()?;
        if after_clock < before_clock {
            return None;
        }
        let (Some(before_layout), Some(after_layout)) = (self.native_layout, other.native_layout) else { return None };
        let (Some(before_trace), Some(after_trace)) = (&self.trace, &other.trace) else { return None };
        if !before_trace.is_consistent() || !after_trace.is_consistent() {
            return None;
        }
        let (Some(before_catalog), Some(after_catalog)) = (&self.native_catalog, &other.native_catalog) else {
            return None;
        };
        let same = self.facts == other.facts
            && before_layout == after_layout
            && before_trace == after_trace
            && before_catalog == after_catalog;
        // This optional observation is not permission to fall back. Changed configuration withholds authority.
        same.then_some(true)
    }
}

fn component<T>(result: Option<Result<T>>) -> (Option<T>, Option<ComponentError>) {
    match result {
        Some(Ok(value)) => (Some(value), None),
        Some(Err(error)) => (None, Some(error.into())),
        None => (None, None),
    }
}
fn has(row: &Rec, name: &str) -> bool {
    row.columns().any(|(field, _)| field == name)
}
fn field_integer(row: Option<&Rec>, name: &str) -> Result<Option<i64>> {
    row.filter(|row| has(row, name))
        .map(|row| crate::read::optional_signed_integer(row, name))
        .transpose()
        .map(Option::flatten)
}
fn field_text(row: Option<&Rec>, name: &str) -> Result<Option<String>> {
    row.filter(|row| has(row, name)).map(|row| crate::read::optional_text(row, name)).transpose().map(Option::flatten)
}
fn field_boolean(row: Option<&Rec>, name: &str) -> Result<Option<bool>> {
    match field_integer(row, name)? {
        None => Ok(None),
        Some(0) => Ok(Some(false)),
        Some(1) => Ok(Some(true)),
        _ => Err(invalid_data()),
    }
}
fn decode_trace(row: &Rec) -> Result<TraceStatus> {
    let flag = field_integer(Some(row), "traceflag")?.ok_or_else(invalid_data)?;
    if flag != 12606 {
        return Err(invalid_data());
    }
    Ok(TraceStatus {
        trace_flag: flag,
        status: field_boolean(Some(row), "status")?.ok_or_else(invalid_data)?,
        global: field_boolean(Some(row), "global")?.ok_or_else(invalid_data)?,
        session: field_boolean(Some(row), "session")?.ok_or_else(invalid_data)?,
    })
}

#[derive(Serialize)]
pub(super) struct RuntimeFragment {
    query: NativeQueryIdentity,
    runtime_stats_id: String,
    plan_id: String,
    runtime_stats_interval_id: String,
    replica_group_id: Option<String>,
    count_executions: String,
}
impl RuntimeFragment {
    pub(super) fn decode(row: &Rec, original: &NativeQueryIdentity) -> Result<Self> {
        let query = decode::query_identity(row)?;
        if query != *original {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(Self {
            query,
            runtime_stats_id: integer(row, "runtime_stats_id")?.to_string(),
            plan_id: integer(row, "plan_id")?.to_string(),
            runtime_stats_interval_id: integer(row, "runtime_stats_interval_id")?.to_string(),
            replica_group_id: crate::read::optional_signed_integer(row, "replica_group_id")?
                .map(|value| value.to_string()),
            count_executions: integer(row, "count_executions")?.to_string(),
        })
    }
}
