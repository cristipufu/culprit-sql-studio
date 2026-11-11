//! PostgreSQL, including the managed services (RDS, Aurora, Azure, Cloud SQL).

use super::*;
use crate::dialect::{NewIndex, included, keys, unique};

pub(super) struct Postgres;

pub(super) fn session_reset_advice(command: &str) -> Option<String> {
    matches!(command, "discard" | "reset").then(|| t!("core.engines.postgres.session_reset"))
}

/// Settings of context `user` in pg_settings: a database's owner sets them for that database (`ALTER DATABASE … SET`),
/// no superuser needed, so they work on a managed service too. Settings of context `superuser` (track_io_timing,
/// pg_stat_statements.track, wal_compression) are left out: a managed service's admin role isn't a superuser.
const USER_SETTINGS: &[&str] = &[
    "random_page_cost",
    "seq_page_cost",
    "cpu_tuple_cost",
    "cpu_index_tuple_cost",
    "cpu_operator_cost",
    "parallel_setup_cost",
    "parallel_tuple_cost",
    "min_parallel_table_scan_size",
    "min_parallel_index_scan_size",
    "effective_cache_size",
    "effective_io_concurrency",
    "work_mem",
    "maintenance_work_mem",
    "hash_mem_multiplier",
    "temp_buffers",
    "max_parallel_workers_per_gather",
    "jit",
    "jit_above_cost",
    "jit_inline_above_cost",
    "jit_optimize_above_cost",
    "default_statistics_target",
    "from_collapse_limit",
    "join_collapse_limit",
    "plan_cache_mode",
    "statement_timeout",
    "lock_timeout",
    "idle_in_transaction_session_timeout",
];

impl EngineSpec for Postgres {
    fn connection_security(&self) -> Option<crate::ConnectionSecuritySpec> {
        Some(connection_security())
    }

    fn partition_tail(&self) -> PartitionTail {
        PartitionTail::FromCatalog
    }
    fn products(&self) -> &'static [ProductSpec] {
        const PRODUCTS: &[ProductSpec] = &[
            ProductSpec::primary(DatabaseProduct::Postgres, "PostgreSQL", key!("core.engines.postgres.plan_format")),
            ProductSpec::variant(
                DatabaseProduct::Timescale,
                "TimescaleDB",
                "TimescaleDB",
                key!("core.engines.postgres.timescale_plan_format"),
            ),
        ];
        PRODUCTS
    }
    fn unused_index_advice(&self, _server: &ServerInfo, table_kind: Option<&str>) -> String {
        if matches!(table_kind, Some("hypertable" | "timescale-chunk" | "timescale-unresolved")) {
            t!("core.engines.postgres.unused_index.hypertable")
        } else {
            t!("core.engines.postgres.unused_index.concurrently")
        }
    }
    fn session_advice(&self, command: &str) -> Option<String> {
        session_reset_advice(command)
    }
    fn statement_metrics(&self) -> StatementMetrics {
        StatementMetrics::pg_stat_statements(WriteLog::Wal)
    }
    fn signal_scope(&self, _server: &ServerInfo, key: &str) -> crate::MeasurementScope {
        if matches!(key, "connections" | "checkpoints" | "checkpoints-requested") {
            crate::MeasurementScope::Server
        } else {
            crate::MeasurementScope::Database
        }
    }

    fn wait_sampling(&self) -> Option<WaitSampling> {
        Some(WaitSampling::Sessions)
    }
    fn sql_syntax(&self) -> SqlSyntax {
        SqlSyntax::Postgres
    }
    fn deadlock_limitation(&self) -> Option<String> {
        Some(t!("core.engines.postgres.deadlocks"))
    }
    fn inactive_index_state(&self, _server: &ServerInfo) -> crate::IndexState {
        crate::IndexState::Invalid
    }
    fn limitation_keys(&self) -> &'static [&'static str] {
        &[
            key!("core.engines.postgres.limitation.pg_stat_statements"),
            key!("core.engines.postgres.limitation.generic_plan"),
            key!("core.engines.postgres.limitation.logs"),
        ]
    }

    fn connect(&self) -> Connect {
        Connect::server("PostgreSQL", "postgres://app@db.example.com:5432/shop", key!("core.engines.postgres.accepts"))
    }

    fn abilities(&self) -> EngineAbilities {
        SERVER
    }

    fn estimated_plans_label(&self) -> &'static str {
        key!("core.engines.estimated_plans.explain")
    }

    fn session_actions(&self) -> Vec<SessionAction> {
        vec![SessionAction::Cancel, SessionAction::Terminate]
    }

    fn refusal(&self, ability: Ability) -> Refusal {
        match ability {
            Ability::LastActualPlan => Refusal::new(
                key!("core.engines.postgres.refusal.last_actual_plan"),
                Some(key!("core.engines.postgres.refusal.last_actual_plan_hint")),
            ),
            Ability::OperatorProgress => Refusal::new(
                key!("core.engines.postgres.refusal.operator_progress"),
                Some(key!("core.engines.refusal.operator_progress_hint")),
            ),
            _ => Refusal::cant(),
        }
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &Postgres
    }

    fn table_dialect(&self, kind: Option<&str>) -> &'static dyn Dialect {
        match kind {
            Some("hypertable" | "timescale-chunk") => &super::timescale::Timescale { per_chunk: true },
            Some("timescale-unresolved") => &super::timescale::Timescale { per_chunk: false },
            _ => self.dialect(),
        }
    }

    fn change_setting(&self, _server: &ServerInfo, name: &str, value: &str) -> SettingChange {
        SettingChange {
            how: t!("core.engines.postgres.change_setting", name = name, value = value),
            sql: Some(format!("ALTER SYSTEM SET {name} = {};\nSELECT pg_reload_conf();", literal(value))),
        }
    }

    fn database_setting(&self, server: &ServerInfo, name: &str, value: &str) -> Option<SettingChange> {
        if !USER_SETTINGS.iter().any(|s| s.eq_ignore_ascii_case(name)) {
            return None;
        }
        Some(SettingChange {
            how: t!("core.engines.postgres.database_setting", name = name, value = value),
            sql: Some(format!(
                "ALTER DATABASE {} SET {name} = {};",
                Dialect::quote(self, &server.database),
                literal(value)
            )),
        })
    }

    fn refresh_statistics(&self, target: &str) -> Fix {
        Fix {
            summary: t!("core.engines.postgres.refresh_statistics.summary"),
            sql: Some(format!(
                "{}\nALTER TABLE {target} SET (autovacuum_analyze_scale_factor = 0.02);",
                self.dialect().refresh_statistics(target)
            )),
            cost: Some(t!("core.engines.postgres.refresh_statistics.cost")),
        }
    }

    fn wait_not_per_statement(&self, _server: &ServerInfo, category: &str) -> String {
        match category {
            "io" => t!("core.engines.postgres.wait_not_per_statement.io"),
            _ => t!("core.engines.postgres.wait_not_per_statement.other", now = t!("core.dashboard.tile.now")),
        }
    }

    fn index_advice_sources(&self, _server: &ServerInfo, _has_workload: bool) -> Option<String> {
        Some(t!("core.engines.postgres.index_advice_sources"))
    }
}

impl Dialect for Postgres {
    fn quote(&self, ident: &str) -> String {
        let simple = ident.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && !ident.starts_with(|c: char| c.is_ascii_digit());
        if simple { ident.to_string() } else { format!("\"{}\"", ident.replace('"', "\"\"")) }
    }

    fn create_index(&self, index: &NewIndex) -> String {
        let mut sql = format!(
            "CREATE {}INDEX CONCURRENTLY {}\n    ON {} ({})",
            unique(index),
            self.quote(index.name),
            index.target,
            keys(self, index)
        );
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

    fn drop_index(&self, schema: &str, _table: &str, index: &str) -> String {
        format!("DROP INDEX CONCURRENTLY {};", self.qualified(&[schema, index]))
    }

    fn refresh_statistics(&self, target: &str) -> String {
        format!("ANALYZE {target};")
    }

    fn end_session(&self, session: &str) -> String {
        format!("SELECT pg_terminate_backend({session});")
    }
}

pub(super) fn connection_security() -> crate::ConnectionSecuritySpec {
    crate::ConnectionSecuritySpec {
        modes: vec![
            crate::ConnectionSecurityMode::new(
                "disable",
                key!("ui.connection_security.mode.disabled"),
                key!("ui.connection_security.description.disabled"),
            )
            .aliases(&["disabled"])
            .permits_plaintext()
            .without_certificates(),
            crate::ConnectionSecurityMode::new(
                "prefer",
                key!("ui.connection_security.mode.preferred"),
                key!("ui.connection_security.description.preferred"),
            )
            .aliases(&["allow"])
            .permits_plaintext(),
            crate::ConnectionSecurityMode::new(
                "require",
                key!("ui.connection_security.mode.required"),
                key!("ui.connection_security.description.required"),
            )
            .ca_verifies_certificate(),
            crate::ConnectionSecurityMode::new(
                "verify-ca",
                key!("ui.connection_security.mode.verify_ca"),
                key!("ui.connection_security.description.verify_ca"),
            ),
            crate::ConnectionSecurityMode::new(
                "verify-full",
                key!("ui.connection_security.mode.verify_identity"),
                key!("ui.connection_security.description.verify_identity"),
            )
            .verifies_identity(),
        ],
        default_mode: "verify-full".into(),
        local_default_mode: "prefer".into(),
        mode_aliases: vec!["sslmode".into()],
        ca_aliases: vec!["sslrootcert".into()],
        mode_option: "sslmode".into(),
        ca_option: "sslrootcert".into(),
        trust_certificate_option: None,
        hostname_option: None,
    }
}
