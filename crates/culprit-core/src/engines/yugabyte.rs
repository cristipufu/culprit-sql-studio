//! YugabyteDB: PostgreSQL's SQL grammar over DocDB's distributed LSM storage.
use super::postgres::Postgres;
use super::*;
use crate::dialect::{NewIndex, included, unique};

pub(super) struct Yugabyte;

/// Parse the node, physical backend and start time that together identify a direct YSQL session. The same parser
/// guards Workspace routing, provider operations and suggested SQL; a plain PID never identifies a cluster session.
pub fn session_target(session: &str) -> crate::Result<(String, i32, String)> {
    let invalid = || crate::Error::invalid(t!("core.engines.yugabyte.session_target"));
    if session.len() > 160 {
        return Err(invalid());
    }
    let mut parts = session.split('/');
    let node = parts.next().ok_or_else(invalid)?.replace('-', "").to_ascii_lowercase();
    let pid = parts.next().and_then(|p| p.parse::<i32>().ok()).filter(|p| *p > 0).ok_or_else(invalid)?;
    let started = parts.next().ok_or_else(invalid)?;
    if parts.next().is_some()
        || node.len() != 32
        || !node.bytes().all(|b| b.is_ascii_hexdigit())
        || chrono::DateTime::parse_from_rfc3339(started)
            .or_else(|_| chrono::DateTime::parse_from_str(started, "%Y-%m-%d %H:%M:%S%.f%#z"))
            .is_err()
    {
        return Err(invalid());
    }
    Ok((node, pid, started.to_string()))
}

impl EngineSpec for Yugabyte {
    fn evidence_presentation(&self) -> EvidencePresentation {
        EvidencePresentation {
            storage_about: key!("ui.beyond_time.storage.about").into(),
            storage_untimed: key!("ui.beyond_time.storage.untimed").into(),
            health_empty: key!("ui.health.empty_hint").into(),
            generic_plan_context: key!("ui.plan_surface.generic_settings").into(),
            statistics_freshness: StatisticsFreshness::Unavailable,
            statistics_absence_known: false,
            workload_windowed_by_default: false,
            deadlock_time_window: false,
            retained_actual_plans: false,
        }
    }

    fn connection_security(&self) -> Option<crate::ConnectionSecuritySpec> {
        Some(super::postgres::connection_security())
    }

    fn partition_tail(&self) -> PartitionTail {
        PartitionTail::FromCatalog
    }
    fn products(&self) -> &'static [ProductSpec] {
        const PRODUCTS: &[ProductSpec] =
            &[ProductSpec::primary(DatabaseProduct::Yugabyte, "YugabyteDB", key!("core.engines.yugabyte.plan_format"))];
        PRODUCTS
    }
    fn unused_index_advice(&self, _server: &ServerInfo, _table_kind: Option<&str>) -> String {
        t!("core.engines.yugabyte.unused_index")
    }
    fn session_advice(&self, command: &str) -> Option<String> {
        super::postgres::session_reset_advice(command)
    }
    fn constraint_validation(&self) -> ConstraintValidation {
        ConstraintValidation::Deferred
    }
    fn statement_metrics(&self) -> StatementMetrics {
        StatementMetrics {
            runtime_note: key!("core.engines.yugabyte.runtime_note").into(),
            ..StatementMetrics::pg_stat_statements(WriteLog::TransactionLog)
        }
    }
    fn signal_scope(&self, _server: &ServerInfo, key: &str) -> crate::MeasurementScope {
        if key == "oldest-transaction" { crate::MeasurementScope::Database } else { crate::MeasurementScope::Server }
    }

    fn wait_sampling(&self) -> Option<WaitSampling> {
        Some(WaitSampling::Sessions)
    }
    fn sql_syntax(&self) -> SqlSyntax {
        SqlSyntax::Postgres
    }
    fn inactive_index_state(&self, _: &ServerInfo) -> crate::IndexState {
        crate::IndexState::Invalid
    }
    fn abilities(&self) -> EngineAbilities {
        EngineAbilities { deadlocks: false, ..SERVER }
    }
    // YSQL asks for one with EXPLAIN, as PostgreSQL does.
    fn estimated_plans_label(&self) -> &'static str {
        key!("core.engines.estimated_plans.explain")
    }
    fn session_actions(&self) -> Vec<SessionAction> {
        vec![SessionAction::Cancel, SessionAction::Terminate]
    }
    fn validate_session_id(&self, session: &str) -> crate::Result<()> {
        session_target(session).map(|_| ())
    }
    fn connect(&self) -> Connect {
        Connect::server(
            "YugabyteDB",
            "yugabyte://yugabyte@db.example.com:5433/yugabyte",
            key!("core.engines.yugabyte.accepts"),
        )
    }
    fn dialect(&self) -> &'static dyn Dialect {
        &Yugabyte
    }
    fn limitation_keys(&self) -> &'static [&'static str] {
        &[
            key!("core.engines.yugabyte.limitation.storage"),
            key!("core.engines.yugabyte.limitation.nodes"),
            key!("core.engines.yugabyte.limitation.generic_plans"),
            key!("core.engines.yugabyte.limitation.sharding"),
        ]
    }
    fn deadlock_limitation(&self) -> Option<String> {
        Some(t!("core.engines.yugabyte.deadlocks"))
    }
    fn refusal(&self, ability: Ability) -> Refusal {
        match ability {
            Ability::Deadlocks => Refusal::new(
                key!("core.engines.yugabyte.deadlocks"),
                Some(key!("core.engines.yugabyte.refusal.deadlocks_hint")),
            ),
            Ability::LastActualPlan => Refusal::new(
                key!("core.engines.yugabyte.refusal.last_actual_plan"),
                Some(key!("core.engines.yugabyte.refusal.last_actual_plan_hint")),
            ),
            Ability::OperatorProgress => Refusal::new(
                key!("core.engines.yugabyte.refusal.operator_progress"),
                Some(key!("core.engines.yugabyte.refusal.operator_progress_hint")),
            ),
            _ => Refusal::new(key!("core.engines.yugabyte.refusal.other"), None),
        }
    }
    fn mutating_function(&self, name: &str) -> bool {
        name.starts_with("yb_")
            && !matches!(
                name,
                "yb_servers"
                    | "yb_servers_metrics"
                    | "yb_lock_status"
                    | "yb_table_properties"
                    | "yb_is_local_table"
                    | "yb_get_local_tserver_uuid"
                    | "yb_server_region"
                    | "yb_server_cloud"
                    | "yb_server_zone"
                    | "yb_hash_code"
                    | "yb_is_database_colocated"
            )
    }
    fn database_setting(&self, server: &ServerInfo, name: &str, value: &str) -> Option<SettingChange> {
        let name = [
            "work_mem",
            "statement_timeout",
            "lock_timeout",
            "idle_in_transaction_session_timeout",
            "default_statistics_target",
        ]
        .into_iter()
        .find(|n| n.eq_ignore_ascii_case(name))?;
        Some(SettingChange {
            how: t!("core.engines.yugabyte.database_setting", name = name),
            sql: Some(format!(
                "ALTER DATABASE {} SET {name} = '{}';",
                self.quote(&server.database),
                value.replace('\'', "''")
            )),
        })
    }
    fn change_setting(&self, server: &ServerInfo, name: &str, value: &str) -> SettingChange {
        self.database_setting(server, name, value).unwrap_or_else(|| SettingChange {
            how: t!("core.engines.yugabyte.change_setting", name = name),
            sql: None,
        })
    }
    fn refresh_statistics(&self, target: &str) -> Fix {
        Fix {
            summary: t!("core.engines.yugabyte.refresh_statistics.summary"),
            sql: Some(self.dialect().refresh_statistics(target)),
            cost: Some(t!("core.engines.yugabyte.refresh_statistics.cost")),
        }
    }
    fn wait_not_per_statement(&self, _: &ServerInfo, _: &str) -> String {
        t!("core.engines.yugabyte.wait_not_per_statement")
    }
    fn index_advice_sources(&self, _: &ServerInfo, has_workload: bool) -> Option<String> {
        Some(if has_workload {
            t!("core.engines.yugabyte.index_advice_sources.workload")
        } else {
            t!("core.engines.yugabyte.index_advice_sources.no_workload")
        })
    }
}

impl Dialect for Yugabyte {
    fn quote(&self, name: &str) -> String {
        Postgres.quote(name)
    }
    fn create_index(&self, index: &NewIndex) -> String {
        // A bare first key defaults to HASH outside enhanced compatibility mode, and cannot serve a range filter.
        let keys = index
            .keys
            .iter()
            .map(|key| {
                let name = crate::model::strip_direction(key);
                let descending = key.to_ascii_uppercase().ends_with(" DESC");
                format!("{} {}", self.quote(name), if descending { "DESC" } else { "ASC" })
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut sql =
            format!("CREATE {}INDEX {}\n    ON {} ({keys})", unique(index), self.quote(index.name), index.target);
        let include = included(self, index);
        if !include.is_empty() {
            sql.push_str(&format!("\n    INCLUDE ({})", include.join(", ")));
        }
        if let Some(filter) = index.filter {
            sql.push_str(&format!("\n    WHERE {filter}"));
        }
        sql.push(';');
        sql
    }
    fn drop_index(&self, schema: &str, _: &str, name: &str) -> String {
        format!("DROP INDEX {};", self.qualified(&[schema, name]))
    }
    fn refresh_statistics(&self, target: &str) -> String {
        format!("ANALYZE {target};")
    }
    fn end_session(&self, session: &str) -> String {
        let Ok((node, pid, started)) = session_target(session) else {
            return "-- Select a node-qualified YSQL session from Live.".into();
        };
        format!(
            "SELECT pg_catalog.pg_terminate_backend(a.pid) FROM pg_catalog.pg_stat_activity a\nWHERE a.pid = {pid} AND a.datname = pg_catalog.current_database()\n  AND a.backend_type = 'client backend' AND a.pid <> pg_catalog.pg_backend_pid()\n  AND COALESCE(a.application_name, '') <> 'culprit' AND a.backend_start = '{}'::timestamptz\n  AND pg_catalog.replace(pg_catalog.yb_get_local_tserver_uuid()::text, '-', '') = '{}';",
            started.replace('\'', "''"),
            node
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_identity_validation_keeps_native_scope_and_rejects_malformed_targets() {
        let node = "3d2d9e10-555b-4ae2-a683-a8402505e416";
        for started in ["2026-10-23T21:00:00.123456Z", "2026-10-23T21:00:00+03:00", "2026-10-23 21:00:00.123456+00"] {
            let id = format!("{node}/42/{started}");
            assert!(Yugabyte.validate_session_id(&id).is_ok());
            let (parsed, pid, at) = session_target(&id).unwrap();
            assert_eq!(parsed, node.replace('-', ""));
            assert_eq!(pid, 42);
            assert_eq!(at, started);
            for other in [Engine::Postgres, Engine::Mssql, Engine::Mysql] {
                assert!(other.spec().validate_session_id(&id).is_err());
                assert!(other.spec().validate_session_id("42").is_ok());
            }
        }
        for id in [
            "42".to_string(),
            format!("{node}/0/2026-10-23T21:00:00Z"),
            format!("{node}/-1/2026-10-23T21:00:00Z"),
            format!("{node}/2147483648/2026-10-23T21:00:00Z"),
            format!("{node}/42/2026-02-31T21:00:00Z"),
            format!("{node}/42/2026-10-23T21:00:00Z/extra"),
            format!("{node}/42/2026-10-23T21:00:00Z'; SELECT 1;--"),
            "x/42/2026-10-23T21:00:00Z".into(),
            "x/".repeat(1000),
        ] {
            assert!(Yugabyte.validate_session_id(&id).is_err(), "{id}");
            assert!(Yugabyte.end_session(&id).starts_with("--"));
        }
    }
    #[test]
    fn indexes_preserve_range_access_and_session_actions_require_node_identity() {
        let keys = vec!["created_at".into(), "id DESC".into()];
        let sql = Yugabyte.create_index(&NewIndex {
            target: "public.orders",
            unique: false,
            name: "by_time",
            keys: &keys,
            include: &[],
            filter: None,
            online: crate::Online::Unknown,
        });
        assert!(sql.contains("(created_at ASC, id DESC)"));
        assert!(!sql.contains("CONCURRENTLY"));
        assert!(Yugabyte.end_session("42").starts_with("--"));
        let sql = Yugabyte.end_session("3d2d9e10-555b-4ae2-a683-a8402505e416/42/2026-10-23T21:00:00Z");
        assert!(
            sql.contains("yb_get_local_tserver_uuid")
                && sql.contains("backend_start")
                && sql.contains("client backend")
        );
    }
}
