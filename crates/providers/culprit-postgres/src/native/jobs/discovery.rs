use super::*;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct View {
    pub capability: TimescaleJobViewCapability,
    pub columns: BTreeMap<String, u32>,
}
impl View {
    pub fn has(&self, name: &str) -> bool {
        self.columns.contains_key(name)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Discovery {
    extension: Option<NativeIdentity>,
    views: Vec<View>,
    role: String,
    visibility: TimescaleJobRunVisibility,
    logging: Option<bool>,
    pub window: TimescaleJobRunWindow,
}
impl Discovery {
    pub fn view(&self, kind: TimescaleJobViewKind) -> Option<&TimescaleJobViewCapability> {
        self.views.iter().find(|view| view.capability.kind == kind).map(|view| &view.capability)
    }
    pub fn available(&self, kind: TimescaleJobViewKind) -> Option<&View> {
        self.views
            .iter()
            .find(|view| view.capability.kind == kind && view.capability.state == TimescaleJobViewState::Available)
    }
    pub fn run_view(&self) -> Option<&View> {
        self.available(TimescaleJobViewKind::History).or_else(|| self.available(TimescaleJobViewKind::Errors))
    }
    pub fn procedure_origin(&self) -> TimescaleJobProcedureOrigin {
        self.run_view()
            .zip(self.extension.as_ref().and_then(|extension| extension.qualifiers.get("version")))
            .map_or(TimescaleJobProcedureOrigin::Unknown, |(view, version)| {
                TimescaleJobProcedureOrigin::for_view(version, view.capability.kind)
            })
    }
    pub fn metadata(&self, writable: Option<bool>) -> TimescaleJobsMetadata {
        TimescaleJobsMetadata {
            extension: self.extension.clone(),
            views: self.views.iter().map(|view| view.capability.clone()).collect(),
            run_view: self.run_view().map(|view| view.capability.kind),
            run_visibility: self.visibility,
            successful_runs_logged: self.logging,
            run_window: self.window.clone(),
            database_writable: writable,
        }
    }
    pub fn unavailable(&self, source: &str, kind: TimescaleJobViewKind) -> StateSourceCoverage {
        if self.extension.is_none() {
            return StateSourceCoverage::not_applicable(source.into());
        }
        let code = if self.view(kind).is_some_and(|view| view.state == TimescaleJobViewState::PermissionDenied) {
            ErrorCode::PermissionDenied
        } else {
            ErrorCode::Unsupported
        };
        StateSourceCoverage::unavailable(source.into(), code, None)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Column {
    name: String,
    oid: u32,
}

pub(super) async fn read<R: QueryRead>(reader: &mut R, database_oid: &str, anchor: &str) -> Result<Discovery> {
    let anchor = canonical_time(anchor)?;
    // PostgreSQL's JSON conversion quotes oid values. Cast to bigint to preserve the numeric column-type contract.
    let sql = format!(
        r#"WITH names(name) AS (VALUES ('jobs'),('job_stats'),('job_history'),('job_errors'))
      SELECT names.name AS view_name,e.oid::text AS extension_oid,e.extversion AS extension_version,
       c.oid::text AS view_oid,pg_catalog.has_table_privilege(c.oid,'SELECT') AS can_select,
       (SELECT pg_catalog.json_agg(pg_catalog.json_build_object('name',a.attname,'oid',a.atttypid::bigint) ORDER BY a.attnum)::text
        FROM pg_catalog.pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped) AS columns,
       current_user::text AS role,(SELECT r.rolsuper FROM pg_catalog.pg_roles r WHERE r.rolname=current_user)
         OR pg_catalog.pg_has_role(d.datdba,'MEMBER') AS database_owner,
       pg_catalog.current_setting('timescaledb.enable_job_execution_logging',true) AS successful_runs_logged,
       {} AS window_from
      FROM names CROSS JOIN pg_catalog.pg_database d
      LEFT JOIN pg_catalog.pg_extension e ON e.extname='timescaledb'
      LEFT JOIN pg_catalog.pg_namespace n ON n.nspname='timescaledb_information'
      LEFT JOIN (pg_catalog.pg_class c JOIN pg_catalog.pg_depend dep ON dep.classid='pg_catalog.pg_class'::regclass
        AND dep.objid=c.oid AND dep.refclassid='pg_catalog.pg_extension'::regclass AND dep.deptype='e')
        ON c.relnamespace=n.oid AND c.relname=names.name AND c.relkind='v' AND dep.refobjid=e.oid
      WHERE d.oid={}::oid ORDER BY names.name"#,
        iso(&format!("'{anchor}'::timestamptz - INTERVAL '{TIMESCALE_JOB_HISTORY_HOURS} hours'")),
        database_oid.parse::<u32>().map_err(|_| invalid())?
    );
    let rows = reader.query(&sql, 5).await?;
    if rows.len() != 4 {
        return Err(invalid());
    }
    let mut result: Option<Discovery> = None;
    for row in rows {
        let kind = match text(&row, "view_name")?.as_str() {
            "jobs" => TimescaleJobViewKind::Jobs,
            "job_stats" => TimescaleJobViewKind::Statistics,
            "job_history" => TimescaleJobViewKind::History,
            "job_errors" => TimescaleJobViewKind::Errors,
            _ => return Err(invalid()),
        };
        let extension = match (row.text("extension_oid"), row.text("extension_version")) {
            (Some(oid), Some(version)) => {
                Some(NativeIdentity::new(oid).qualified("database_oid", database_oid).qualified("version", version))
            }
            (None, None) => None,
            _ => return Err(invalid()),
        };
        let role = text(&row, "role")?;
        let visibility = if extension.is_none() {
            TimescaleJobRunVisibility::Unknown
        } else if flag(&row, "database_owner")? {
            TimescaleJobRunVisibility::DatabaseOwner
        } else {
            TimescaleJobRunVisibility::JobOwners
        };
        let logging = if extension.is_none() {
            None
        } else {
            match row.value("successful_runs_logged") {
                Some(None) => None,
                Some(Some("on" | "true" | "t" | "1")) => Some(true),
                Some(Some("off" | "false" | "f" | "0")) => Some(false),
                _ => return Err(invalid()),
            }
        };
        let window = TimescaleJobRunWindow { from: text(&row, "window_from")?, until: anchor.clone() };
        let view = decode_view(&row, kind, extension.as_ref())?;
        let entry = result.get_or_insert_with(|| Discovery {
            extension: extension.clone(),
            views: Vec::new(),
            role: role.clone(),
            visibility,
            logging,
            window: window.clone(),
        });
        if entry.extension != extension
            || entry.role != role
            || entry.visibility != visibility
            || entry.logging != logging
            || entry.window != window
            || entry.views.iter().any(|old| old.capability.kind == kind)
        {
            return Err(invalid());
        }
        entry.views.push(view);
    }
    let result = result.ok_or_else(invalid)?;
    if !result.metadata(None).is_valid() {
        return Err(invalid());
    }
    Ok(result)
}
fn decode_view(row: &Row, kind: TimescaleJobViewKind, extension: Option<&NativeIdentity>) -> Result<View> {
    let Some(id) = row.text("view_oid") else {
        if row.text("columns").is_some() || row.value("can_select") != Some(None) {
            return Err(invalid());
        }
        return Ok(View {
            capability: TimescaleJobViewCapability { kind, state: TimescaleJobViewState::Missing, identity: None },
            columns: BTreeMap::new(),
        });
    };
    let extension = extension.ok_or_else(invalid)?;
    let columns: Vec<Column> = serde_json::from_str(&text(row, "columns")?).map_err(|_| invalid())?;
    if columns.len() > 128 {
        return Err(invalid());
    }
    let mut types = BTreeMap::new();
    for column in columns {
        if types.insert(column.name, column.oid).is_some() {
            return Err(invalid());
        }
    }
    let fingerprint = digest(&serde_json::to_vec(&types).map_err(|_| invalid())?);
    let identity = NativeIdentity::new(id)
        .qualified("view", kind.native_name())
        .qualified("extension_oid", &extension.id)
        .qualified("columns", fingerprint);
    let state = if !flag(row, "can_select")? {
        TimescaleJobViewState::PermissionDenied
    } else if !supported(kind, &types) {
        TimescaleJobViewState::UnsupportedColumns
    } else {
        TimescaleJobViewState::Available
    };
    Ok(View { capability: TimescaleJobViewCapability { kind, state, identity: Some(identity) }, columns: types })
}
fn supported(kind: TimescaleJobViewKind, columns: &BTreeMap<String, u32>) -> bool {
    let required: &[(&str, &[u32])] = match kind {
        TimescaleJobViewKind::Jobs => &[
            ("job_id", &[23]),
            ("proc_schema", &[19, 25]),
            ("proc_name", &[19, 25]),
            ("owner", &[19, 25, 4096]),
            ("scheduled", &[16]),
            ("schedule_interval", &[1186]),
            ("max_runtime", &[1186]),
            ("max_retries", &[23]),
            ("retry_period", &[1186]),
        ],
        TimescaleJobViewKind::Statistics => &[("job_id", &[23])],
        TimescaleJobViewKind::History => &[
            ("id", &[20, 23]),
            ("job_id", &[23]),
            ("start_time", &[1184]),
            ("finish_time", &[1184]),
            ("succeeded", &[16]),
        ],
        TimescaleJobViewKind::Errors => &[("job_id", &[23]), ("start_time", &[1184]), ("finish_time", &[1184])],
    };
    let optional: &[(&str, &[u32])] = match kind {
        TimescaleJobViewKind::Jobs => &[
            ("fixed_schedule", &[16]),
            ("initial_start", &[1184]),
            ("next_start", &[1184]),
            ("hypertable_schema", &[19, 25]),
            ("hypertable_name", &[19, 25]),
        ],
        TimescaleJobViewKind::Statistics => &[
            ("job_status", &[25]),
            ("last_run_status", &[25]),
            ("last_run_started_at", &[1184]),
            ("last_successful_finish", &[1184]),
            ("last_run_duration", &[1186]),
            ("next_start", &[1184]),
            ("total_runs", &[20]),
            ("total_successes", &[20]),
            ("total_failures", &[20]),
        ],
        TimescaleJobViewKind::History | TimescaleJobViewKind::Errors => {
            &[("pid", &[23]), ("sqlerrcode", &[25]), ("proc_schema", &[19, 25]), ("proc_name", &[19, 25])]
        }
    };
    required.iter().all(|(key, allowed)| columns.get(*key).is_some_and(|oid| allowed.contains(oid)))
        && optional.iter().all(|(key, allowed)| columns.get(*key).is_none_or(|oid| allowed.contains(oid)))
}
