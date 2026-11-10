//! SQL Server and Azure SQL.

use super::*;
use crate::dialect::{NewIndex, Online, included, keys, unique};
use crate::model::Capability;

pub(super) struct Mssql;

impl EngineSpec for Mssql {
    fn connection_security(&self) -> Option<crate::ConnectionSecuritySpec> {
        Some(connection_security())
    }

    fn products(&self) -> &'static [ProductSpec] {
        const PRODUCTS: &[ProductSpec] =
            &[ProductSpec::primary(DatabaseProduct::Mssql, "SQL Server", key!("core.engines.mssql.plan_format"))];
        PRODUCTS
    }
    fn hide_index(&self, _server: &ServerInfo, schema: &str, table: &str, index: &str) -> Option<String> {
        Some(format!("ALTER INDEX {} ON {} DISABLE;", self.quote(index), self.qualified(&[schema, table])))
    }
    fn unused_index_advice(&self, _server: &ServerInfo, _table_kind: Option<&str>) -> String {
        t!("core.engines.mssql.unused_index")
    }
    fn session_advice(&self, _command: &str) -> Option<String> {
        Some(t!("core.engines.mssql.session_advice"))
    }
    fn statement_metrics(&self) -> StatementMetrics {
        StatementMetrics {
            compilation_history: true,
            lock_time: true,
            runtime_note: key!("core.engines.mssql.runtime_note").into(),
            runtime_cache_note: Some(key!("core.engines.mssql.runtime_cache_note").into()),
            errors_note: Some(key!("core.engines.mssql.errors_note").into()),
            ..StatementMetrics::new(
                PlanningStep::Compiling,
                ReadUnit::Pages,
                Some(WriteRows::Rows),
                WriteLog::TransactionLog,
            )
        }
    }
    fn signal_scope(&self, server: &ServerInfo, key: &str) -> crate::MeasurementScope {
        let azure = server.on(crate::Platform::AzureSqlDatabase);
        let shared = matches!(
            key,
            "batch-requests"
                | "compiles-per-batch"
                | "recompiles-per-batch"
                | "page-life"
                | "new-connections"
                | "pooled-reuse"
        ) || (!azure
            && matches!(key, "signal-wait" | "threadpool-waits" | "threadpool-wait-ms" | "grant-wait-ms"));
        if shared { crate::MeasurementScope::Server } else { crate::MeasurementScope::Database }
    }

    fn scoped_signal_label(&self, signal: &crate::Signal) -> Option<crate::i18n::Message> {
        let whole = if signal.scope == crate::SignalScope::Sampled { "instance" } else { "server" };
        Some(match signal.key.as_str() {
            "batch-requests" => crate::message!("provider.mssql.signal.batch_requests", whole = "server"),
            "compiles-per-batch" => crate::message!("provider.mssql.signal.plans_compiled", whole = whole),
            "recompiles-per-batch" => crate::message!("provider.mssql.signal.plans_recompiled", whole = "server"),
            "page-life" => crate::message!("provider.mssql.signal.page_life", whole = "server"),
            "new-connections" => crate::message!("provider.mssql.signal.new_connections", whole = "server"),
            "pooled-reuse" => crate::message!("provider.mssql.signal.pooled_reuse", whole = "server"),
            "threadpool-wait-ms" => crate::message!("provider.mssql.signal.threadpool_wait_ms", whole = "instance"),
            "grant-wait-ms" => crate::message!("provider.mssql.signal.grant_wait_ms", whole = "instance"),
            _ => return None,
        })
    }

    fn wait_sampling(&self) -> Option<WaitSampling> {
        Some(WaitSampling::Counters)
    }
    fn sql_syntax(&self) -> SqlSyntax {
        SqlSyntax::Mssql
    }
    fn inactive_index_state(&self, _server: &ServerInfo) -> crate::IndexState {
        crate::IndexState::Disabled
    }
    fn limitation_keys(&self) -> &'static [&'static str] {
        &[
            key!("core.engines.mssql.limitation.query_store"),
            key!("core.engines.mssql.limitation.estimated_plan"),
            key!("core.engines.mssql.limitation.host"),
        ]
    }

    fn connect(&self) -> Connect {
        Connect::server(
            "SQL Server",
            "Server=tcp:shop.database.windows.net;Database=shop;User ID=app",
            key!("core.engines.mssql.accepts"),
        )
    }

    fn abilities(&self) -> EngineAbilities {
        EngineAbilities { last_actual_plan: true, operator_progress: true, ..SERVER }
    }

    fn estimated_plans_label(&self) -> &'static str {
        key!("core.model.capability.showplan")
    }

    fn session_actions(&self) -> Vec<SessionAction> {
        vec![SessionAction::Terminate]
    }

    fn waiting_state(&self) -> &'static str {
        "suspended"
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &Mssql
    }

    fn change_setting(&self, _server: &ServerInfo, name: &str, value: &str) -> SettingChange {
        SettingChange {
            how: t!("core.engines.mssql.change_setting", name = name, value = value),
            sql: Some(format!("EXEC sp_configure '{name}', {};\nRECONFIGURE;", literal(value))),
        }
    }

    fn refresh_statistics(&self, target: &str) -> Fix {
        Fix {
            summary: t!("core.engines.mssql.refresh_statistics.summary"),
            sql: Some(self.dialect().refresh_statistics(target)),
            cost: Some(t!("core.engines.mssql.refresh_statistics.cost")),
        }
    }

    fn wait_not_per_statement(&self, _server: &ServerInfo, _category: &str) -> String {
        t!("core.engines.mssql.wait_not_per_statement")
    }

    fn index_advice_sources(&self, server: &ServerInfo, _has_workload: bool) -> Option<String> {
        (!server.has(Capability::MissingIndexes)).then(|| t!("core.engines.mssql.index_advice_sources"))
    }
}

impl Dialect for Mssql {
    fn quote(&self, ident: &str) -> String {
        format!("[{}]", ident.replace(']', "]]"))
    }

    fn create_index(&self, index: &NewIndex) -> String {
        let mut sql = format!(
            "CREATE {}NONCLUSTERED INDEX {}\n    ON {} ({})",
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
        match index.online {
            Online::Yes => sql.push_str("\n    WITH (ONLINE = ON);"),
            Online::No => sql.push(';'),
            Online::Unknown => sql.push_str(
                ";\n-- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.",
            ),
        }
        sql
    }

    fn drop_index(&self, schema: &str, table: &str, index: &str) -> String {
        format!("DROP INDEX {} ON {};", self.quote(index), self.qualified(&[schema, table]))
    }

    fn refresh_statistics(&self, target: &str) -> String {
        format!("UPDATE STATISTICS {target};")
    }

    fn end_session(&self, session: &str) -> String {
        format!("KILL {session};")
    }
}

pub(super) fn connection_security() -> crate::ConnectionSecuritySpec {
    crate::ConnectionSecuritySpec {
        modes: vec![
            crate::ConnectionSecurityMode::new(
                "false",
                key!("ui.connection_security.mode.optional"),
                key!("ui.connection_security.description.optional"),
            )
            .aliases(&["no", "0", "optional"])
            .permits_plaintext()
            .permits_trust_override(),
            crate::ConnectionSecurityMode::new(
                "true",
                key!("ui.connection_security.mode.required"),
                key!("ui.connection_security.description.mssql_required"),
            )
            .aliases(&["yes", "1", "required", "mandatory"])
            .verifies_identity()
            .permits_trust_override(),
            crate::ConnectionSecurityMode::new(
                "strict",
                key!("ui.connection_security.mode.strict"),
                key!("ui.connection_security.description.strict"),
            )
            .verifies_identity(),
        ],
        default_mode: "true".into(),
        local_default_mode: "true".into(),
        mode_aliases: vec!["encrypt".into()],
        ca_aliases: vec!["trustServerCertificateCA".into()],
        mode_option: "encrypt".into(),
        ca_option: "trustServerCertificateCA".into(),
        trust_certificate_option: Some("trustServerCertificate".into()),
        hostname_option: Some("hostNameInCertificate".into()),
    }
}
