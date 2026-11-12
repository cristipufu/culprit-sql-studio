//! MySQL and MariaDB, told apart by the server's product where they differ.

use super::*;
use crate::dialect::{NewIndex, keys, unique};
use crate::model::Capability;

pub(super) struct Mysql;

fn version_pair(server: &ServerInfo) -> Option<(u32, u32)> {
    let mut parts = server.version.split(['.', '-']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn index_hiding(server: &ServerInfo) -> Option<&'static str> {
    let version = version_pair(server)?;
    if server.is_mariadb() {
        (version >= (10, 6)).then_some("IGNORED")
    } else {
        (version >= (8, 0)).then_some("INVISIBLE")
    }
}

impl EngineSpec for Mysql {
    fn evidence_presentation(&self) -> EvidencePresentation {
        EvidencePresentation {
            storage_about: key!("ui.beyond_time.storage.about").into(),
            storage_untimed: key!("ui.beyond_time.storage.untimed").into(),
            health_empty: key!("ui.health.empty_hint").into(),
            generic_plan_context: key!("ui.plan_surface.generic_settings").into(),
            statistics_freshness: StatisticsFreshness::PerTable,
            statistics_absence_known: false,
            workload_windowed_by_default: false,
            deadlock_time_window: true,
            retained_actual_plans: false,
        }
    }

    fn connection_security(&self) -> Option<crate::ConnectionSecuritySpec> {
        Some(connection_security())
    }

    fn partition_tail(&self) -> PartitionTail {
        PartitionTail::FromCatalog
    }
    fn products(&self) -> &'static [ProductSpec] {
        const PRODUCTS: &[ProductSpec] = &[
            ProductSpec::primary(DatabaseProduct::Mysql, "MySQL", key!("core.engines.mysql.plan_format")),
            ProductSpec::variant(
                DatabaseProduct::Mariadb,
                "MariaDB",
                "MariaDB",
                key!("core.engines.mysql.mariadb_plan_format"),
            ),
        ];
        PRODUCTS
    }
    fn unused_index_advice(&self, server: &ServerInfo, _table_kind: Option<&str>) -> String {
        if version_pair(server).is_none() {
            t!("core.engines.mysql.unused_index.unknown_version")
        } else if index_hiding(server) == Some("IGNORED") {
            t!("core.engines.mysql.unused_index.ignored")
        } else if server.is_mariadb() {
            t!("core.engines.mysql.unused_index.mariadb_cant_hide")
        } else if index_hiding(server).is_none() {
            t!("core.engines.mysql.unused_index.mysql_cant_hide")
        } else {
            t!("core.engines.mysql.unused_index.invisible")
        }
    }
    fn hide_index(&self, server: &ServerInfo, schema: &str, table: &str, index: &str) -> Option<String> {
        let state = index_hiding(server)?;
        Some(format!("ALTER TABLE {} ALTER INDEX {} {state};", self.qualified(&[schema, table]), self.quote(index)))
    }
    fn constraint_validation(&self) -> ConstraintValidation {
        ConstraintValidation::Unsupported
    }
    fn statement_metrics(&self) -> StatementMetrics {
        StatementMetrics {
            lock_time: true,
            runtime_note: key!("core.engines.mysql.runtime_note").into(),
            runtime_histogram_note: Some(key!("core.engines.mysql.runtime_histogram_note").into()),
            missing_costs_note: Some(key!("core.engines.mysql.missing_costs_note").into()),
            ..StatementMetrics::new(PlanningStep::Planning, ReadUnit::RowsExamined, None, WriteLog::TransactionLog)
        }
    }
    fn signal_scope(&self, _server: &ServerInfo, key: &str) -> crate::MeasurementScope {
        if key == "oldest-transaction" { crate::MeasurementScope::Database } else { crate::MeasurementScope::Server }
    }

    fn scoped_signal_label(&self, signal: &crate::Signal) -> Option<crate::i18n::Message> {
        Some(match signal.key.as_str() {
            "history-length" => crate::message!("provider.mysql.signal.history_length"),
            "row-lock-time" => crate::message!("provider.mysql.signal.row_lock_time"),
            "row-lock-waits" if signal.scope == crate::SignalScope::Sampled => {
                crate::message!("provider.mysql.signal.row_lock_waits_sampled")
            }
            _ => return None,
        })
    }

    fn wait_sampling(&self) -> Option<WaitSampling> {
        Some(WaitSampling::Counters)
    }
    fn sql_syntax(&self) -> SqlSyntax {
        SqlSyntax::Mysql
    }
    fn deadlock_limitation(&self) -> Option<String> {
        Some(t!("core.engines.mysql.deadlocks"))
    }
    fn inactive_index_state(&self, server: &ServerInfo) -> crate::IndexState {
        if server.is_mariadb() { crate::IndexState::Ignored } else { crate::IndexState::Invisible }
    }
    fn limitation_keys(&self) -> &'static [&'static str] {
        &[
            key!("core.engines.mysql.limitation.digests"),
            key!("core.engines.mysql.limitation.innodb_status"),
            key!("core.engines.mysql.limitation.products"),
        ]
    }

    fn connect(&self) -> Connect {
        Connect::server("MySQL / MariaDB", "mysql://app@db.example.com:3306/shop", key!("core.engines.mysql.accepts"))
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
                key!("core.engines.mysql.refusal.last_actual_plan"),
                Some(key!("core.engines.mysql.refusal.last_actual_plan_hint")),
            ),
            Ability::OperatorProgress => Refusal::new(
                key!("core.engines.mysql.refusal.operator_progress"),
                Some(key!("core.engines.refusal.operator_progress_hint")),
            ),
            _ => Refusal::cant(),
        }
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &Mysql
    }

    fn change_setting(&self, server: &ServerInfo, name: &str, value: &str) -> SettingChange {
        if !server.is_mariadb() && crate::version_at_least(&server.version, [8, 0, 0]) {
            return SettingChange {
                how: t!("core.engines.mysql.change_setting.persist", name = name, value = value),
                sql: Some(format!("SET PERSIST {name} = {};", literal(value))),
            };
        }
        SettingChange {
            how: t!("core.engines.mysql.change_setting.global", name = name, value = value),
            sql: Some(format!("SET GLOBAL {name} = {};", literal(value))),
        }
    }

    fn refresh_statistics(&self, target: &str) -> Fix {
        Fix {
            summary: t!("core.engines.mysql.refresh_statistics"),
            sql: Some(self.dialect().refresh_statistics(target)),
            cost: None,
        }
    }

    fn wait_not_per_statement(&self, server: &ServerInfo, category: &str) -> String {
        match category {
            "lock" | "cpu" => t!("core.engines.mysql.wait_not_per_statement.unrecorded"),
            _ => t!("core.engines.mysql.wait_not_per_statement.server_wide", server = server.name()),
        }
    }

    fn index_advice_sources(&self, server: &ServerInfo, _has_workload: bool) -> Option<String> {
        Some(if server.has(Capability::PerformanceSchema) {
            t!("core.engines.mysql.index_advice_sources.performance_schema", server = server.name())
        } else {
            t!("core.engines.mysql.index_advice_sources.logged", server = server.name())
        })
    }
}

impl Dialect for Mysql {
    fn quote(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn create_index(&self, index: &NewIndex) -> String {
        // InnoDB has no INCLUDE: secondary indexes already carry the primary key, so extra columns would have to become
        // keys. No partial indexes either.
        format!(
            "CREATE {}INDEX {}\n    ON {} ({})\n    ALGORITHM=INPLACE LOCK=NONE;",
            unique(index),
            self.quote(index.name),
            index.target,
            keys(self, index)
        )
    }

    fn drop_index(&self, schema: &str, table: &str, index: &str) -> String {
        // MariaDB's DROP INDEX takes no ALGORITHM/LOCK options; ALTER TABLE takes them (comma-separated) on both.
        format!(
            "ALTER TABLE {} DROP INDEX {}, ALGORITHM=INPLACE, LOCK=NONE;",
            self.qualified(&[schema, table]),
            self.quote(index)
        )
    }

    fn refresh_statistics(&self, target: &str) -> String {
        format!("ANALYZE TABLE {target};")
    }

    fn end_session(&self, session: &str) -> String {
        format!("KILL {session};")
    }
}

pub(super) fn connection_security() -> crate::ConnectionSecuritySpec {
    crate::ConnectionSecuritySpec {
        modes: vec![
            crate::ConnectionSecurityMode::new(
                "DISABLED",
                key!("ui.connection_security.mode.disabled"),
                key!("ui.connection_security.description.disabled"),
            )
            .aliases(&["disable", "false", "off"])
            .permits_plaintext()
            .without_certificates(),
            crate::ConnectionSecurityMode::new(
                "PREFERRED",
                key!("ui.connection_security.mode.preferred"),
                key!("ui.connection_security.description.preferred"),
            )
            .aliases(&["prefer", "allow"])
            .permits_plaintext(),
            crate::ConnectionSecurityMode::new(
                "REQUIRED",
                key!("ui.connection_security.mode.required"),
                key!("ui.connection_security.description.required"),
            )
            .aliases(&["require", "true", "on"]),
            crate::ConnectionSecurityMode::new(
                "VERIFY_CA",
                key!("ui.connection_security.mode.verify_ca"),
                key!("ui.connection_security.description.verify_ca"),
            ),
            crate::ConnectionSecurityMode::new(
                "VERIFY_IDENTITY",
                key!("ui.connection_security.mode.verify_identity"),
                key!("ui.connection_security.description.verify_identity"),
            )
            .aliases(&["verify-full"])
            .verifies_identity(),
        ],
        default_mode: "VERIFY_IDENTITY".into(),
        local_default_mode: "PREFERRED".into(),
        mode_aliases: vec!["sslmode".into(), "ssl-mode".into(), "ssl_mode".into()],
        ca_aliases: vec!["sslrootcert".into(), "ssl-ca".into(), "ssl_ca".into()],
        mode_option: "ssl-mode".into(),
        ca_option: "ssl-ca".into(),
        trust_certificate_option: None,
        hostname_option: None,
    }
}
