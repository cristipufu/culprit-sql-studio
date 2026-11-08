//! SQLite: a file the application opens itself, which Culprit opens read-only. No server to hold sessions, waits or
//! deadlocks; EXPLAIN QUERY PLAN has the access path only; a query log is one period, with none to compare.

use super::*;
use crate::dialect::{NewIndex, keys, unique};

pub(super) struct Sqlite;

impl EngineSpec for Sqlite {
    fn products(&self) -> &'static [ProductSpec] {
        const PRODUCTS: &[ProductSpec] =
            &[ProductSpec::primary(DatabaseProduct::Sqlite, "SQLite", key!("core.engines.sqlite.plan_format"))];
        PRODUCTS
    }
    fn unused_index_advice(&self, _server: &ServerInfo, _table_kind: Option<&str>) -> String {
        t!("core.engines.sqlite.unused_index")
    }
    fn statement_metrics(&self) -> StatementMetrics {
        StatementMetrics {
            runtime_note: key!("core.engines.sqlite.runtime_note").into(),
            ..StatementMetrics::new(
                PlanningStep::Planning,
                ReadUnit::Pages,
                Some(WriteRows::Returned),
                WriteLog::TransactionLog,
            )
        }
    }
    fn wait_sampling(&self) -> Option<WaitSampling> {
        None
    }
    fn sql_syntax(&self) -> SqlSyntax {
        SqlSyntax::Sqlite
    }
    fn inactive_index_state(&self, _server: &ServerInfo) -> crate::IndexState {
        crate::IndexState::Active
    }
    fn limitation_keys(&self) -> &'static [&'static str] {
        &[
            key!("core.engines.sqlite.limitation.connections"),
            key!("core.engines.sqlite.limitation.library"),
            key!("core.engines.sqlite.limitation.access_paths"),
            key!("core.engines.sqlite.limitation.wal"),
        ]
    }

    fn connect(&self) -> Connect {
        Connect {
            choice: "SQLite".into(),
            file: true,
            example: "~/code/shop/db/production.sqlite3".into(),
            accepts: key!("core.engines.sqlite.accepts").into(),
        }
    }

    fn abilities(&self) -> EngineAbilities {
        EngineAbilities {
            sessions: false,
            wait_sample: false,
            deadlocks: false,
            measures_runs: false,
            plan_estimates: false,
            writes: false,
            periods: false,
            last_actual_plan: false,
            operator_progress: false,
            sign_in: false,
        }
    }

    fn estimated_plans_label(&self) -> &'static str {
        key!("core.engines.estimated_plans.explain_query_plan")
    }

    fn refusal(&self, ability: Ability) -> Refusal {
        match ability {
            Ability::Sessions | Ability::OperatorProgress => Refusal::new(
                key!("core.engines.sqlite.refusal.sessions"),
                Some(key!("core.engines.sqlite.refusal.sessions_hint")),
            ),
            Ability::Deadlocks => Refusal::new(
                key!("core.engines.sqlite.refusal.deadlocks"),
                Some(key!("core.engines.sqlite.refusal.deadlocks_hint")),
            ),
            Ability::WaitSample => Refusal::new(
                key!("core.engines.sqlite.refusal.wait_sample"),
                Some(key!("core.engines.sqlite.refusal.wait_sample_hint")),
            ),
            Ability::MeasuresRuns | Ability::LastActualPlan | Ability::PlanEstimates => Refusal::new(
                key!("core.engines.sqlite.refusal.plans"),
                Some(key!("core.engines.sqlite.refusal.plans_hint")),
            ),
            Ability::Writes => Refusal::new(
                key!("core.engines.sqlite.refusal.writes"),
                Some(key!("core.engines.sqlite.refusal.writes_hint")),
            ),
            Ability::Periods => Refusal::new(
                key!("core.engines.sqlite.refusal.periods"),
                Some(key!("core.engines.sqlite.refusal.periods_hint")),
            ),
            Ability::SignIn => Refusal::new(key!("core.engines.sqlite.refusal.sign_in"), None),
        }
    }

    fn dialect(&self) -> &'static dyn Dialect {
        &Sqlite
    }

    // A pragma that lives in the file (journal_mode = WAL) is set once; one that lives in the connection (synchronous,
    // cache_size) has to be set by the application each time it opens the file.
    fn change_setting(&self, _server: &ServerInfo, name: &str, value: &str) -> SettingChange {
        SettingChange {
            how: t!("core.engines.sqlite.change_setting", name = name, value = value),
            sql: Some(format!("PRAGMA {name} = {value};")),
        }
    }

    fn refresh_statistics(&self, target: &str) -> Fix {
        Fix {
            summary: t!("core.engines.sqlite.refresh_statistics.summary"),
            sql: Some(self.dialect().refresh_statistics(target)),
            cost: Some(t!("core.engines.sqlite.refresh_statistics.cost")),
        }
    }

    fn wait_not_per_statement(&self, _server: &ServerInfo, _category: &str) -> String {
        t!("core.engines.sqlite.wait_not_per_statement")
    }

    fn index_advice_sources(&self, _server: &ServerInfo, has_workload: bool) -> Option<String> {
        Some(if has_workload {
            t!("core.engines.sqlite.index_advice_sources.logged")
        } else {
            t!("core.engines.sqlite.index_advice_sources.no_history")
        })
    }
}

impl Dialect for Sqlite {
    fn quote(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn create_index(&self, index: &NewIndex) -> String {
        // No INCLUDE and no online build: SQLite builds an index under the database's write lock, so writers wait.
        // SQLite qualifies the index, not the ON table. The target was quoted by this dialect: a dot inside a
        // quoted identifier (including one with doubled quotes) is part of the name, not a schema separator.
        let mut quoted = false;
        let split = index.target.char_indices().find_map(|(at, ch)| {
            if ch == '"' {
                quoted = !quoted;
            }
            (ch == '.' && !quoted).then_some(at)
        });
        let (name, target) = match split {
            Some(at) => (format!("{}.{}", &index.target[..at], self.quote(index.name)), &index.target[at + 1..]),
            None => (self.quote(index.name), index.target),
        };
        let mut sql =
            format!("CREATE {}INDEX IF NOT EXISTS {}\n    ON {} ({})", unique(index), name, target, keys(self, index));
        if let Some(filter) = index.filter {
            sql.push_str(&format!("\n    WHERE {filter}"));
        }
        sql.push_str(";\n-- SQLite builds an index under the database's write lock: writers wait until it finishes.");
        sql
    }

    fn drop_index(&self, schema: &str, _table: &str, index: &str) -> String {
        format!("DROP INDEX IF EXISTS {};", self.qualified(&[schema, index]))
    }

    fn refresh_statistics(&self, target: &str) -> String {
        format!("ANALYZE {target};")
    }

    fn end_session(&self, _session: &str) -> String {
        "-- SQLite has no sessions to end: the application that holds the write lock has to finish its transaction."
            .into()
    }
}
