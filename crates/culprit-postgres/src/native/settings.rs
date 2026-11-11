//! One allowlisted configuration reader for interactive scans and background capture.
use super::*;
use culprit_core::{MeasurementScope, Setting, telemetry::*};

pub(crate) const SETTINGS: &[&str] = &[
    "pg_stat_statements.max",
    "pg_stat_statements.track",
    "pg_stat_statements.track_planning",
    "shared_buffers",
    "wal_buffers",
    "work_mem",
    "maintenance_work_mem",
    "effective_cache_size",
    "random_page_cost",
    "seq_page_cost",
    "effective_io_concurrency",
    "max_connections",
    "autovacuum",
    "autovacuum_vacuum_scale_factor",
    "autovacuum_analyze_scale_factor",
    "autovacuum_vacuum_threshold",
    "autovacuum_vacuum_max_threshold",
    "autovacuum_analyze_threshold",
    "autovacuum_multixact_freeze_max_age",
    "autovacuum_freeze_max_age",
    "jit",
    "jit_above_cost",
    "track_io_timing",
    "track_wal_io_timing",
    "log_lock_waits",
    "default_statistics_target",
];
pub(crate) const ADVICE: &[&str] = &["wal_compression", "constraint_exclusion", "max_slot_wal_keep_size"];
const COLLECTION: &[&str] = &[
    "max_parallel_workers_per_gather",
    "statement_timeout",
    "lock_timeout",
    "default_transaction_read_only",
    "application_name",
    "track_activities",
    "stats_fetch_consistency",
];
pub(crate) fn query(names: &[&str], limit: usize) -> String {
    let names = names.iter().map(|name| format!("'{}'", name.replace("'", "''"))).collect::<Vec<_>>().join(", ");
    format!(
        "SELECT name, setting, unit, boot_val, source, context, pending_restart FROM pg_catalog.pg_settings WHERE name IN ({names}) ORDER BY name LIMIT {limit}"
    )
}
pub(crate) fn capture_query(limit: usize) -> String {
    query(&SETTINGS.iter().chain(ADVICE).chain(COLLECTION).copied().collect::<Vec<_>>(), limit)
}
pub(crate) fn decode(row: &Row) -> Result<SettingObservation> {
    let name = text(row, "name")?;
    if !SETTINGS.iter().chain(ADVICE).chain(COLLECTION).any(|allowed| *allowed == name) {
        return Err(invalid());
    }
    let source = text(row, "source")?;
    let context = text(row, "context")?;
    let scope =
        if source == "session" || matches!(context.as_str(), "user" | "superuser" | "backend" | "superuser-backend") {
            MeasurementScope::Session
        } else {
            MeasurementScope::Server
        };
    let default_value = row.text("boot_val");
    let observation = SettingObservation {
        source: "pg_settings".into(),
        namespace: "pg_catalog.pg_settings".into(),
        name,
        scope,
        nature: SettingNature::Configuration,
        value: None,
        value_in_use: row.text("setting"),
        value_for_secondary: None,
        unit: row.text("unit"),
        native_context: Some(context),
        native_source: Some(source.clone()),
        pending_restart: Some(flag(row, "pending_restart")?),
        native_is_default: Some(source == "default"),
        default_source: if default_value.is_some() {
            SettingDefaultSource::NativeValue
        } else {
            SettingDefaultSource::Unknown
        },
        default_value,
    };
    if !observation.is_valid() {
        return Err(invalid());
    }
    Ok(observation)
}
pub(crate) fn project(observation: SettingObservation) -> Setting {
    let format = |value: &str| crate::catalog::pretty_setting(value, observation.unit.as_deref());
    Setting {
        name: observation.name,
        value: observation.value_in_use.as_deref().or(observation.value.as_deref()).map(format).unwrap_or_default(),
        default_value: observation.default_value.as_deref().map(format),
        scope: if observation.scope == MeasurementScope::Session { "session" } else { "server" }.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_allowlist_excludes_credentials_and_retains_native_scope_and_units() {
        let sql = capture_query(1000);
        assert!(!sql.contains("primary_conninfo"));
        assert!(!sql.contains("password"));
        assert!(sql.contains("track_io_timing"));
        let row = Row::from_fields(
            [
                ("name", Some("work_mem")),
                ("setting", Some("4096")),
                ("unit", Some("kB")),
                ("boot_val", Some("4096")),
                ("source", Some("session")),
                ("context", Some("user")),
                ("pending_restart", Some("f")),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.map(str::to_owned))),
        );
        let observation = decode(&row).unwrap();
        assert_eq!(observation.scope, MeasurementScope::Session);
        assert_eq!(observation.unit.as_deref(), Some("kB"));
        assert_eq!(observation.default_source, SettingDefaultSource::NativeValue);
        assert_eq!(project(observation).value, "4MB");
    }
}
