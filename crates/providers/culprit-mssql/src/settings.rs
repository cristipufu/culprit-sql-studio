//! Native setting families own queries and interpretation once; app and export project the same observations.
use crate::{
    read::{NativeRead, ReadError, invalid_data, optional_text, text},
    value::{Rec, Val},
};
use culprit_core::{
    Engine, MeasurementScope, Result, Setting,
    telemetry::{CollectionStop, SettingDefaultSource, SettingNature, SettingObservation},
};

mod frame;

const SCOPED: &str = r#"c.*, CONVERT(nvarchar(128),c.value) AS value_text,
 CONVERT(nvarchar(128),c.value_for_secondary) AS secondary_value_text FROM sys.database_scoped_configurations c
 OPTION(MAXDOP 1)"#;
const SERVER: &str = r#"name, CONVERT(nvarchar(128),value) AS value,
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
fn query(kind: Kind, context_columns: &str) -> String {
    match kind {
        Kind::Database => crate::database_properties::query_with_context(
            context_columns,
            if context_columns.is_empty() { "" } else { frame::EXECUTION_PARAMETERS },
        ),
        Kind::Scoped => format!("SELECT {context_columns}{SCOPED}"),
        Kind::Server => format!("SELECT {context_columns}{SERVER}"),
        Kind::QueryStore => crate::query_store::catalog_sql::options_with_context(context_columns),
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
    Ok(read_family(reader, kind, limit, None).await?.values)
}
pub(crate) async fn read_current<R: NativeRead>(
    reader: &mut R,
    kind: Kind,
    limit: usize,
    owner: &crate::local_source::Context,
) -> Result<Vec<SettingObservation>> {
    Ok(read_family(reader, kind, limit, Some(owner)).await?.values)
}

struct FamilyRead {
    values: Vec<SettingObservation>,
    properties: Option<culprit_core::telemetry::DatabasePropertiesObservation>,
}
async fn read_family<R: NativeRead>(
    reader: &mut R,
    kind: Kind,
    limit: usize,
    owner: Option<&crate::local_source::Context>,
) -> Result<FamilyRead> {
    if kind == Kind::Database {
        let read = match owner {
            None => crate::database_properties::read(reader).await?,
            Some(owner) => {
                let rows = frame::read(reader, &query(kind, frame::COLUMNS), 2, owner).await?;
                crate::database_properties::decode_read(&rows)?
            }
        };
        let point = read.properties?;
        if owner.is_some_and(|owner| {
            point.native_database_id != owner.database_id.to_string()
                || point.database != owner.activity.database.database
                || point.writable().is_some_and(|writable| Some(writable) != owner.activity.writable)
        }) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let values = database_settings(&point)?;
        if values.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        return Ok(FamilyRead { values, properties: Some(point) });
    }
    let rows = match owner {
        Some(owner) => frame::read(reader, &query(kind, frame::COLUMNS), limit.saturating_add(1), owner).await?,
        None => reader.query(&query(kind, ""), vec![], limit.saturating_add(1)).await.map_err(ReadError::into_error)?,
    };
    let values = match kind {
        Kind::Database => return Err(invalid_data()),
        Kind::QueryStore => {
            let [row] = rows.as_slice() else { return Err(invalid_data()) };
            query_store_options(row)?
        }
        Kind::Scoped | Kind::Server => rows.iter().map(|row| named_row(row, kind)).collect::<Result<_>>()?,
    };
    if values.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    Ok(FamilyRead { values: ordered(values)?, properties: None })
}

fn database_settings(
    point: &culprit_core::telemetry::DatabasePropertiesObservation,
) -> Result<Vec<SettingObservation>> {
    let bit = |value: Option<bool>| value.map(|value| u8::from(value).to_string());
    let configuration = SettingNature::Configuration;
    let state = SettingNature::State;
    ordered(
        [
            ("compatibility_level", point.compatibility_level.map(|value| value.to_string()), configuration),
            ("is_read_committed_snapshot_on", bit(point.read_committed_snapshot), configuration),
            ("snapshot_isolation_state_desc", point.snapshot_isolation.description.clone(), state),
            ("is_auto_shrink_on", bit(point.auto_shrink), configuration),
            ("is_broker_enabled", bit(point.broker_enabled), configuration),
            ("is_in_standby", bit(point.in_standby), state),
            ("delayed_durability_desc", point.delayed_durability.description.clone(), configuration),
            ("containment_desc", point.containment.description.clone(), configuration),
            ("is_encrypted", bit(point.encrypted), configuration),
            ("is_ledger_on", bit(point.ledger_enabled), configuration),
            ("is_change_feed_enabled", bit(point.change_feed_enabled), configuration),
            ("is_published", bit(point.published), state),
            ("is_subscribed", bit(point.subscribed), state),
            ("is_merge_published", bit(point.merge_published), state),
            ("is_distributor", bit(point.distributor), state),
            ("is_accelerated_database_recovery_on", bit(point.accelerated_recovery_enabled), configuration),
            ("is_auto_create_stats_on", bit(point.auto_create_statistics), configuration),
            ("is_auto_update_stats_on", bit(point.auto_update_statistics), configuration),
            ("is_auto_update_stats_async_on", bit(point.auto_update_statistics_async), configuration),
            ("is_parameterization_forced", bit(point.forced_parameterization), configuration),
            ("is_query_store_on", bit(point.query_store_enabled), state),
            ("page_verify_option_desc", point.page_verify.description.clone(), configuration),
            ("recovery_model_desc", point.recovery_model.description.clone(), configuration),
            (
                "target_recovery_time_in_seconds",
                point.target_recovery_time_seconds.map(|value| value.to_string()),
                configuration,
            ),
        ]
        .into_iter()
        .map(|(name, value, nature)| base(Kind::Database, name.into(), value, nature))
        .collect(),
    )
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

pub(crate) struct CollectedSettings {
    pub settings: Vec<Setting>,
    pub database_properties: Option<Box<culprit_core::telemetry::DatabasePropertiesRead>>,
}

pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    target: culprit_core::telemetry::CollectionTarget,
    azure_sql_database: bool,
    options: Option<&Rec>,
    coverage: &mut culprit_core::Coverage,
    grant: &str,
) -> CollectedSettings {
    let mut reader = crate::read::ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    collect_with(&mut reader, target, azure_sql_database, options, coverage, grant).await
}

async fn collect_with<R: NativeRead>(
    reader: &mut R,
    target: culprit_core::telemetry::CollectionTarget,
    azure_sql_database: bool,
    options: Option<&Rec>,
    coverage: &mut culprit_core::Coverage,
    grant: &str,
) -> CollectedSettings {
    let started = std::time::SystemTime::now();
    let mut settings = Vec::new();
    let mut properties = Err(CollectionStop::InvalidPage.error());
    let mut failed = false;
    let mut stopped: Option<culprit_core::Error> = None;
    for kind in Kind::ALL {
        if (kind == Kind::Server && azure_sql_database) || (kind == Kind::QueryStore && options.is_none()) {
            coverage.not_applicable(kind.source(), culprit_core::message!("provider.mssql.settings.not_applicable"));
            continue;
        }
        let result = if kind == Kind::QueryStore {
            // Probe already read these options through the shared Query Store source.
            query_store_options(options.expect("the applicable options were checked above"))
                .map(|values| FamilyRead { values, properties: None })
        } else if let Some(error) = &stopped {
            Err(error.clone())
        } else {
            read_family(reader, kind, MAX_SETTINGS_PER_SOURCE, None).await
        };
        let result = result.and_then(|read| {
            if read
                .properties
                .as_ref()
                .is_some_and(|point| target.database().is_none_or(|database| point.database != database.database))
            {
                return Err(CollectionStop::SourceChanged.error());
            }
            Ok(read)
        });
        if kind == Kind::Database {
            properties =
                result.as_ref().map_err(Clone::clone).and_then(|read| read.properties.clone().ok_or_else(invalid_data));
        }
        let result = result.map_err(|error| {
            if stopped.is_none() && crate::read::stops_remaining_reads(&error) {
                stopped = Some(error.clone());
            }
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(grant.to_owned())
            } else {
                error
            }
        });
        match coverage.read_rows(kind.source(), result.map(|read| read.values)) {
            Some(rows) => settings.extend(rows.iter().filter_map(application)),
            None => failed = true,
        }
    }
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        coverage.source_failed(&crate::sources::SETTINGS, &CollectionStop::CancellationUnconfirmed.error());
        properties = Err(CollectionStop::CancellationUnconfirmed.error());
    } else if failed {
        coverage.partial(&crate::sources::SETTINGS, culprit_core::message!("provider.mssql.settings.partial"));
    } else {
        coverage.completed(&crate::sources::SETTINGS, settings.len());
    }
    let database_properties = retain_properties(target, properties, started, std::time::SystemTime::now(), coverage);
    CollectedSettings { settings, database_properties }
}

fn retain_properties(
    target: culprit_core::telemetry::CollectionTarget,
    result: Result<culprit_core::telemetry::DatabasePropertiesObservation>,
    started: std::time::SystemTime,
    finished: std::time::SystemTime,
    coverage: &mut culprit_core::Coverage,
) -> Option<Box<culprit_core::telemetry::DatabasePropertiesRead>> {
    use culprit_core::telemetry::*;
    let (source, observation, context) = match result {
        Ok(point) => {
            coverage.completed(&crate::sources::DATABASE_PROPERTIES, 1);
            let context = NativeReadContext {
                target: target.clone(),
                partition: SourcePartition::Target,
                observed_from: point.observed_at.clone(),
                observed_until: point.observed_at.clone(),
                database_writable: point.writable(),
            };
            (
                StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1),
                Some(Box::new(point)),
                Ok(ObservationReadContext::Native { context: Box::new(context) }),
            )
        }
        Err(error) => {
            coverage.source_failed(&crate::sources::DATABASE_PROPERTIES, &error);
            (
                StateSourceCoverage::unavailable(
                    DATABASE_PROPERTIES_SOURCE.into(),
                    error.code,
                    Some(CollectionStop::from_error(&error)),
                ),
                None,
                ObservationReadContext::collector(started, finished),
            )
        }
    };
    let result = context.and_then(|context| {
        let read = DatabasePropertiesRead { target, context, source, observation, read_complete: true };
        read.is_valid().then_some(Box::new(read)).ok_or_else(invalid_data)
    });
    match result {
        Ok(read) => Some(read),
        Err(error) => {
            coverage.source_failed(&crate::sources::DATABASE_PROPERTIES, &error);
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
