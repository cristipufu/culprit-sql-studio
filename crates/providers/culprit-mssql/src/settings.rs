//! Native setting families own queries and interpretation once; app and export project the same observations.
use crate::{
    read::{NativeRead, ReadError, invalid_data, optional_text, text},
    value::{Rec, Val},
};
use culprit_core::{
    Engine, MeasurementScope, Result, Setting,
    telemetry::{CollectionStop, SettingDefaultSource, SettingNature, SettingObservation},
};

const DATABASE: &str = r#"SELECT compatibility_level, is_read_committed_snapshot_on, snapshot_isolation_state_desc,
 is_auto_create_stats_on, is_auto_update_stats_on, is_auto_update_stats_async_on,
 is_parameterization_forced, is_query_store_on, page_verify_option_desc, recovery_model_desc,
 target_recovery_time_in_seconds FROM sys.databases WHERE database_id=DB_ID() OPTION(MAXDOP 1)"#;
const SCOPED: &str = r#"SELECT c.*, CONVERT(nvarchar(128),c.value) AS value_text,
 CONVERT(nvarchar(128),c.value_for_secondary) AS secondary_value_text FROM sys.database_scoped_configurations c
 OPTION(MAXDOP 1)"#;
const SERVER: &str = r#"SELECT name, CONVERT(nvarchar(128),value) AS value,
 CONVERT(nvarchar(128),value_in_use) AS value_in_use FROM sys.configurations
 WHERE name IN ('cost threshold for parallelism','max degree of parallelism','optimize for ad hoc workloads','max server memory (MB)')
 OPTION(MAXDOP 1)"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Database,
    Scoped,
    Server,
    QueryStore,
}
const MAX_SETTINGS_PER_SOURCE: usize = 4096;
impl Kind {
    pub const ALL: [Self; 4] = [Self::Database, Self::Scoped, Self::Server, Self::QueryStore];
    pub fn source(self) -> &'static culprit_core::Source {
        use crate::sources;
        match self {
            Self::Database => &sources::DATABASE_SETTINGS,
            Self::Scoped => &sources::SCOPED_SETTINGS,
            Self::Server => &sources::SERVER_SETTINGS,
            Self::QueryStore => &sources::QUERY_STORE_SETTINGS,
        }
    }
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Database => "sys.databases",
            Self::Scoped => "sys.database_scoped_configurations",
            Self::Server => "sys.configurations",
            Self::QueryStore => "sys.database_query_store_options",
        }
    }
    pub fn scope(self) -> MeasurementScope {
        if self == Self::Server { MeasurementScope::Server } else { MeasurementScope::Database }
    }
}
fn base(kind: Kind, name: String, value: Option<String>, nature: SettingNature) -> SettingObservation {
    SettingObservation {
        source: kind.source().name.into(),
        namespace: kind.namespace().into(),
        name,
        value,
        value_in_use: None,
        value_for_secondary: None,
        unit: None,
        native_context: None,
        native_source: None,
        pending_restart: None,
        scope: kind.scope(),
        nature,
        native_is_default: None,
        default_value: None,
        default_source: SettingDefaultSource::Unknown,
    }
}
fn scalar(value: &Val) -> Result<Option<String>> {
    Ok(match value {
        Val::Null => None,
        Val::Int(value) => Some(value.to_string()),
        Val::Bool(value) => Some(u8::from(*value).to_string()),
        Val::Float(value) if value.is_finite() => Some(value.to_string()),
        Val::Text(value) => Some(value.clone()),
        Val::Bytes(value) => Some(crate::value::hex(value)),
        _ => return Err(invalid_data()),
    })
}
pub(crate) fn query_store_options(row: &Rec) -> Result<Vec<SettingObservation>> {
    ordered(wide_row(row, Kind::QueryStore, query_store_nature)?)
}
fn query_store_nature(name: &str) -> SettingNature {
    match name {
        "desired_state"
        | "desired_state_desc"
        | "flush_interval_seconds"
        | "interval_length_minutes"
        | "max_storage_size_mb"
        | "stale_query_threshold_days"
        | "max_plans_per_query"
        | "query_capture_mode"
        | "query_capture_mode_desc"
        | "size_based_cleanup_mode"
        | "size_based_cleanup_mode_desc"
        | "wait_stats_capture_mode"
        | "wait_stats_capture_mode_desc"
        | "capture_policy_stale_threshold_hours"
        | "capture_policy_execution_count"
        | "capture_policy_total_compile_cpu_time_ms"
        | "capture_policy_total_execution_cpu_time_ms" => SettingNature::Configuration,
        "actual_state" | "actual_state_desc" | "readonly_reason" => SettingNature::State,
        "current_storage_size_mb" => SettingNature::Measurement,
        _ => SettingNature::Unknown,
    }
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, kind: Kind, limit: usize) -> Result<Vec<SettingObservation>> {
    let sql = match kind {
        Kind::Database => DATABASE,
        Kind::Scoped => SCOPED,
        Kind::Server => SERVER,
        Kind::QueryStore => crate::query_store::catalog_sql::OPTIONS,
    };
    let rows = reader.query(sql, vec![], limit.saturating_add(1)).await.map_err(ReadError::into_error)?;
    let values = match kind {
        Kind::Database => {
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            wide_row(row, kind, |name| {
                if matches!(name, "snapshot_isolation_state_desc" | "is_query_store_on") {
                    SettingNature::State
                } else {
                    SettingNature::Configuration
                }
            })?
        }
        Kind::QueryStore => {
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            query_store_options(row)?
        }
        Kind::Scoped | Kind::Server => rows.iter().map(|row| named_row(row, kind)).collect::<Result<_>>()?,
    };
    if values.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    ordered(values)
}
fn wide_row(row: &Rec, kind: Kind, nature: impl Fn(&str) -> SettingNature) -> Result<Vec<SettingObservation>> {
    row.columns().map(|(name, value)| Ok(base(kind, name.into(), scalar(value)?, nature(name)))).collect()
}
fn named_row(row: &Rec, kind: Kind) -> Result<SettingObservation> {
    let field = if kind == Kind::Scoped { "value_text" } else { "value" };
    let mut value = base(kind, text(row, "name")?, optional_text(row, field)?, SettingNature::Configuration);
    if kind == Kind::Scoped {
        value.value_for_secondary = optional_text(row, "secondary_value_text")?;
        value.native_is_default = native_default_flag(row)?;
        if value.native_is_default == Some(true) && value.value.is_some() {
            value.default_value = value.value.clone();
            value.default_source = SettingDefaultSource::NativeFlag;
        }
    } else {
        value.value_in_use = optional_text(row, "value_in_use")?;
        value.default_value = Engine::Mssql.spec().default_setting_value(&value.name).map(str::to_string);
        if value.default_value.is_some() {
            value.default_source = SettingDefaultSource::EngineSpec;
        }
    }
    Ok(value)
}
fn native_default_flag(row: &Rec) -> Result<Option<bool>> {
    if row.columns().any(|(name, _)| name == "is_value_default") {
        crate::read::optional_boolean(row, "is_value_default")
    } else {
        Ok(None)
    }
}

fn ordered(mut values: Vec<SettingObservation>) -> Result<Vec<SettingObservation>> {
    let mut names = std::collections::BTreeSet::new();
    if values.iter().any(|value| !value.is_valid() || !names.insert(value.name.clone())) {
        return Err(invalid_data());
    }
    // Stable ordering makes replay and comparisons independent of native catalog ordering.
    values.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(values)
}
pub(crate) fn application(value: &SettingObservation) -> Option<Setting> {
    let observed = if value.namespace == Kind::Server.namespace() {
        value.value_in_use.as_ref()?
    } else {
        value.value.as_ref()?
    };
    Some(Setting {
        name: if value.namespace == Kind::QueryStore.namespace() {
            format!("query_store_options.{}", value.name)
        } else {
            value.name.clone()
        },
        value: observed.clone(),
        default_value: value.default_value.clone(),
        scope: match value.scope {
            MeasurementScope::Database => "database",
            MeasurementScope::Server => "server",
            _ => return None,
        }
        .into(),
    })
}

pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    azure_sql_database: bool,
    options: Option<&Rec>,
    coverage: &mut culprit_core::Coverage,
    grant: &str,
) -> Vec<Setting> {
    let mut reader = crate::read::ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    collect_with(&mut reader, azure_sql_database, options, coverage, grant).await
}

async fn collect_with<R: NativeRead>(
    reader: &mut R,
    azure_sql_database: bool,
    options: Option<&Rec>,
    coverage: &mut culprit_core::Coverage,
    grant: &str,
) -> Vec<Setting> {
    let mut settings = Vec::new();
    let mut failed = false;
    let mut stopped = false;
    for kind in Kind::ALL {
        if (kind == Kind::Server && azure_sql_database) || (kind == Kind::QueryStore && options.is_none()) {
            coverage.not_applicable(kind.source(), culprit_core::message!("provider.mssql.settings.not_applicable"));
            continue;
        }
        let result = if stopped {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else if kind == Kind::QueryStore {
            // Probe already read these options through the shared Query Store source.
            query_store_options(options.expect("the applicable options were checked above"))
        } else {
            read(reader, kind, MAX_SETTINGS_PER_SOURCE).await
        };
        let result = result.map_err(|error| {
            stopped |= CollectionStop::from_error(&error) == CollectionStop::CancellationUnconfirmed;
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(grant.to_owned())
            } else {
                error
            }
        });
        match coverage.read_rows(kind.source(), result) {
            Some(rows) => settings.extend(rows.iter().filter_map(application)),
            None => failed = true,
        }
    }
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        coverage.source_failed(&crate::sources::SETTINGS, &CollectionStop::CancellationUnconfirmed.error());
    } else if failed {
        coverage.partial(&crate::sources::SETTINGS, culprit_core::message!("provider.mssql.settings.partial"));
    } else {
        coverage.completed(&crate::sources::SETTINGS, settings.len());
    }
    settings
}

#[cfg(test)]
pub(crate) mod tests;
