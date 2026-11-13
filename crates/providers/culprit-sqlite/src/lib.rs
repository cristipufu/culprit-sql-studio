//! SQLite provider. A SQLite database is a file, not a server: Culprit opens it read-only and reads its schema,
//! settings and plans. SQLite keeps no statement statistics, sessions or waits, so those parts of a snapshot are
//! missing capabilities with the reason, never zeros. See `docs/providers/sqlite.md`.

mod collect;
mod file;
mod native;
pub mod plan;
mod read;
pub mod sources;
mod telemetry;
#[cfg(test)]
mod words;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use culprit_core::{
    Ability, ConnectionSpec, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, Provider, Result, ResultSet,
    ServerInfo, Snapshot, SnapshotOptions, t,
};
#[cfg(test)]
use rusqlite::Connection;

pub use plan::plan_from_rows;

/// Identity of this crate's collector and native-plan parser, including a build revision when supplied.
pub fn component_version() -> culprit_core::ComponentVersion {
    culprit_core::ComponentVersion::package(
        "culprit-sqlite",
        env!("CARGO_PKG_VERSION"),
        option_env!("CULPRIT_BUILD_REVISION").or(option_env!("GITHUB_SHA")),
    )
}

pub struct SqliteProvider {
    path: PathBuf,
    spec: ConnectionSpec,
    reader: read::FileReader,
}

impl SqliteProvider {
    pub fn new(spec: &ConnectionSpec) -> Result<Self> {
        if spec.ssh.is_some() || spec.transport.is_some() {
            return Err(Error::new(ErrorCode::Unsupported, t!("provider.sqlite.open.no_ssh")));
        }
        let path = spec
            .database
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error::invalid(t!("provider.sqlite.open.path_required")))?;
        let path = culprit_core::expand_home(path);
        Ok(SqliteProvider { reader: read::FileReader::new(path.clone()), path, spec: spec.clone() })
    }
}

#[cfg(test)]
fn open(path: &std::path::Path) -> Result<Connection> {
    let conn = read::open(path)?;
    conn.busy_timeout(Duration::from_secs(2)).map_err(|e| map_error(&e))?;
    conn.execute_batch("PRAGMA query_only = 1;").map_err(|e| map_error(&e))?;
    Ok(conn)
}

pub(crate) fn map_error(e: &rusqlite::Error) -> Error {
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
        return Error::new(ErrorCode::Timeout, culprit_core::message!("provider.sqlite.error.interrupted"))
            .with_hint(culprit_core::message!("provider.sqlite.error.interrupted.hint"));
    }
    let message = e.to_string();
    let code = match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => ErrorCode::Timeout,
        Some(rusqlite::ErrorCode::ReadOnly) => ErrorCode::PermissionDenied,
        Some(rusqlite::ErrorCode::NotADatabase) => ErrorCode::InvalidInput,
        Some(rusqlite::ErrorCode::CannotOpen) => ErrorCode::ConnectionFailed,
        _ => ErrorCode::QueryFailed,
    };
    let error = Error::new(code, message);
    match code {
        ErrorCode::Timeout => error.with_hint(culprit_core::message!("provider.sqlite.error.busy.hint")),
        ErrorCode::PermissionDenied => error.with_hint(culprit_core::message!("provider.sqlite.error.read_only.hint")),
        _ => error,
    }
}

/// The blocking task running SQLite's calls ended without an answer (`error`).
pub(crate) fn task_stopped(error: &dyn std::fmt::Display) -> Error {
    Error::internal(t!("provider.sqlite.error.task_stopped", error = error.to_string()))
}

/// The pool of statement workers is closed.
pub(crate) fn execution_unavailable() -> Error {
    Error::internal(t!("provider.sqlite.error.unavailable"))
}

const STATEMENT_TIMEOUT: Duration = culprit_core::limits::QUERY_TIMEOUT;
#[cfg(test)]
async fn interactive<T: Send + 'static>(
    path: PathBuf,
    budget: Duration,
    operation: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    read::FileReader::new(path).execute(budget, move |conn, _| operation(conn)).await
}

#[async_trait]
impl Provider for SqliteProvider {
    fn collector_version(&self) -> culprit_core::ComponentVersion {
        component_version()
    }

    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn source_shutdown(&self) -> culprit_core::telemetry::SourceShutdown {
        self.reader.shutdown()
    }

    fn telemetry_source(
        &self,
        request: &culprit_core::telemetry::CollectionRequest,
    ) -> Result<Box<dyn culprit_core::telemetry::TelemetrySource>> {
        telemetry::source(self.reader.clone(), self.path.clone(), request)
    }

    async fn probe(&self) -> Result<ServerInfo> {
        self.reader
            .execute(culprit_core::limits::OBSERVE_TIMEOUT, move |conn, path| {
                let (have, missing) = collect::capabilities(conn, collect::whole_file_read(conn).map(|_| ()));
                Ok(collect::server_info(conn, path, have, missing))
            })
            .await
    }

    async fn snapshot(&self, connection: &str, options: &SnapshotOptions) -> Result<Snapshot> {
        let (connection, environment) = (connection.to_string(), self.spec.environment);
        let mut snapshot = self
            .reader
            .with_handle(culprit_core::limits::OBSERVE_TIMEOUT, move |conn, path, handle| {
                collect::snapshot_with_handle(conn, path, &connection, environment, Instant::now(), handle)
            })
            .await?;
        let mut coverage = culprit_core::Coverage::default();
        if options.collect_shared_resources {
            coverage.shared_resource_details_unavailable(Engine::Sqlite);
        }
        if options.collect_request_memory {
            coverage.request_memory_unavailable(Engine::Sqlite);
        }
        coverage.append_to(&mut snapshot);
        Ok(snapshot)
    }

    async fn explain(&self, sql: &str, mode: ExplainMode) -> Result<Plan> {
        if mode == ExplainMode::Analyze {
            return Err(Engine::Sqlite.refused(Ability::MeasuresRuns));
        }
        let statement = culprit_core::sql::single_statement(sql, Engine::Sqlite)?.to_string();
        self.reader.execute(culprit_core::limits::OBSERVE_TIMEOUT, move |conn, _| plan::explain(conn, &statement)).await
    }

    async fn live(&self) -> Result<LiveState> {
        Err(Engine::Sqlite.refused(Ability::Sessions))
    }

    async fn run(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let statement = culprit_core::sql::single_statement(sql, Engine::Sqlite)?.to_string();
        self.reader.execute(STATEMENT_TIMEOUT, move |conn, _| collect::run(conn, &statement, limit)).await
    }

    async fn preview(&self, sql: &str, limit: usize) -> Result<ResultSet> {
        let statement = culprit_core::sql::single_statement(sql, Engine::Sqlite)?.to_string();
        self.reader
            .execute(culprit_core::limits::PREVIEW_TIMEOUT, move |conn, _| collect::run(conn, &statement, limit))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{NewIndex, Online};

    // Expensive before its first row, but finite: a broken interrupt must fail the test without hanging runtime shutdown.
    const SLOW: &str =
        "WITH RECURSIVE c(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM c WHERE x < 10000000) SELECT count(*) FROM c";

    fn query_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("culprit-query-{name}-{}.sqlite", std::process::id()));
        drop(Connection::open(&path).unwrap());
        path
    }

    #[tokio::test]
    async fn a_requested_shared_profile_is_explicitly_unsupported_while_file_evidence_stays_readable() {
        let path = query_file("shared-profile");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE profile_fixture(id INTEGER PRIMARY KEY,value TEXT);INSERT INTO profile_fixture(value) VALUES('owned fixture');").unwrap();
        drop(connection);
        let original = std::fs::read(&path).unwrap();
        let provider = SqliteProvider::new(&ConnectionSpec {
            engine: Some(Engine::Sqlite),
            database: Some(path.to_str().unwrap().into()),
            ..Default::default()
        })
        .unwrap();
        let ordinary = provider.snapshot("file", &SnapshotOptions::default()).await.unwrap();
        assert!(
            ordinary.sources.iter().all(|source| source.source != culprit_core::source_id::SHARED_RESOURCE_DETAILS)
        );
        let options =
            SnapshotOptions { collect_shared_resources: true, collect_request_memory: true, ..Default::default() };
        let requested = provider.snapshot("file", &options).await.unwrap();
        let outcome = requested
            .sources
            .iter()
            .find(|source| source.source == culprit_core::source_id::SHARED_RESOURCE_DETAILS)
            .unwrap();
        assert_eq!(outcome.state, culprit_core::SourceState::Unavailable);
        assert!(outcome.reason_message.is_some());
        let outcome = requested
            .sources
            .iter()
            .find(|source| source.source == culprit_core::source_id::CURRENT_REQUEST_MEMORY)
            .unwrap();
        assert_eq!(outcome.state, culprit_core::SourceState::Unavailable);
        assert!(outcome.reason_message.is_some());
        assert!(requested.tables.iter().any(|table| table.name == "profile_fixture"));
        assert_eq!(ordinary.tables.len(), requested.tables.len());
        assert!(requested.pressure.is_none() && requested.resources.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(provider.source_shutdown(), culprit_core::telemetry::SourceShutdown::Quiescent);
        drop(provider);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn execution_deadlines_interrupt_work_before_the_first_result_row() {
        let path = query_file("deadline");
        let result = interactive(path.clone(), Duration::from_millis(10), |conn| collect::run(conn, SLOW, 1));
        let error = tokio::time::timeout(Duration::from_secs(2), result).await.unwrap().unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn dropping_a_caller_stops_its_blocking_query() {
        let path = query_file("cancellation");
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finished, stopped) = tokio::sync::oneshot::channel();
        let query = tokio::spawn(interactive(path.clone(), STATEMENT_TIMEOUT, move |conn| {
            let _ = started.send(());
            let result = collect::run(conn, SLOW, 1);
            let _ = finished.send(result.as_ref().err().map(|e| e.code));
            result
        }));
        ready.await.unwrap();
        query.abort();
        let _ = query.await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), stopped).await.unwrap().unwrap(),
            Some(ErrorCode::Timeout)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_identity_uses_the_canonical_path_and_detects_replacements() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("culprit-sqlite-identity-{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("shop.sqlite");
        let read = |path: &std::path::Path| {
            let mut conn = open(path).unwrap();
            collect::snapshot(&mut conn, path, "shop", culprit_core::Environment::Local, Instant::now()).unwrap()
        };
        drop(Connection::open(&path).unwrap());
        let first = read(&path);
        let alias = read(&dir.join(".").join("shop.sqlite"));
        assert!(first.identity.is_some());
        assert!(first.same_database(&alias));
        #[cfg(unix)]
        {
            let link = dir.join("alias.sqlite");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(first.same_database(&read(&link)));
        }
        let other = dir.join("other.sqlite");
        drop(Connection::open(&other).unwrap());
        assert!(!first.same_database(&read(&other)));
        // Keep the old inode alive so the new file cannot reuse it.
        std::fs::rename(&path, dir.join("original.sqlite")).unwrap();
        drop(Connection::open(&path).unwrap());
        let replacement = read(&path);
        assert!(!first.same_database(&replacement));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn collection_outcomes_distinguish_empty_catalog_and_inapplicable_wal() {
        let mut conn = Connection::open_in_memory().unwrap();
        let read = |conn: &mut Connection| {
            collect::snapshot(
                conn,
                std::path::Path::new("example.sqlite"),
                "example",
                culprit_core::Environment::Local,
                Instant::now(),
            )
            .unwrap()
        };
        let empty = read(&mut conn);
        assert_eq!(empty.source_outcome("catalog").unwrap().state, culprit_core::SourceState::Empty);
        assert_eq!(empty.source_outcome("wal_file").unwrap().state, culprit_core::SourceState::NotApplicable);
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT); CREATE INDEX items_name ON items(name);",
        )
        .unwrap();
        let populated = read(&mut conn);
        assert_eq!(populated.source_outcome("catalog").unwrap().state, culprit_core::SourceState::Collected);
        assert!(populated.tables.iter().any(|t| t.name == "items"));
        assert!(populated.indexes.iter().any(|i| i.name == "items_name"));
        assert!(populated.source_problem(&["catalog"]).is_none());
    }

    #[test]
    fn an_unreadable_statistics_source_locks_only_its_capability() {
        use rusqlite::hooks::{AuthAction, Authorization};
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE items (name TEXT); CREATE INDEX items_name ON items(name); ANALYZE;").unwrap();
        conn.authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
            if matches!(context.action, AuthAction::Read { table_name: "sqlite_stat1", .. }) {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))
        .unwrap();
        let snapshot = collect::snapshot(
            &mut conn,
            std::path::Path::new("example.sqlite"),
            "example",
            culprit_core::Environment::Local,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(snapshot.source_outcome("statistics").unwrap().state, culprit_core::SourceState::Unavailable);
        assert!(!snapshot.server.has(culprit_core::Capability::Statistics));
        assert!(snapshot.server.has(culprit_core::Capability::TableStats));
        assert!(snapshot.tables.iter().any(|t| t.name == "items"));
        assert!(snapshot.server.missing.iter().any(|m| m.capability == culprit_core::Capability::Statistics));
    }

    #[test]
    fn index_fixes_execute_in_main_and_attached_schemas() {
        let conn = Connection::open_in_memory().unwrap();
        let dialect = Engine::Sqlite.dialect();
        let attached = "audit.\"logs";
        conn.execute_batch(&format!("ATTACH ':memory:' AS {};", dialect.quote(attached))).unwrap();
        let table = "or.\"ders";
        let column = "customer.\"id";
        let keys = vec![format!("{column} DESC")];
        let filter = format!("{} IS NOT NULL", dialect.quote(column));
        for schema in ["main", attached] {
            conn.execute_batch(&format!(
                "CREATE TABLE {} ({} INTEGER);",
                dialect.qualified(&[schema, table]),
                dialect.quote(column)
            ))
            .unwrap();
        }
        // Main, attached, and unqualified targets all use SQLite's grammar. Dots and quotes inside identifiers must
        // survive the move of the schema from the table to the index, as must unique, descending and partial keys.
        for (schema, qualified, name) in
            [("main", true, "ix.\"main"), (attached, true, "ix.\"attached"), ("main", false, "ix.\"unqualified")]
        {
            let target = if qualified { dialect.qualified(&[schema, table]) } else { dialect.quote(table) };
            let sql = dialect.create_index(&NewIndex {
                target: &target,
                unique: true,
                name,
                keys: &keys,
                include: &[],
                filter: Some(&filter),
                online: Online::No,
            });
            conn.execute_batch(&sql).unwrap_or_else(|error| panic!("{sql}\n{error}"));
            let found: (String, bool) = conn
                .query_row(
                    &format!(
                        "SELECT tbl_name, sql LIKE 'CREATE UNIQUE INDEX%WHERE%' FROM {}.sqlite_schema WHERE name = ?",
                        dialect.quote(schema)
                    ),
                    [name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(found, (table.to_string(), true), "{sql}");
            let descending: bool = conn
                .query_row(
                    &format!("PRAGMA {}.index_xinfo({});", dialect.quote(schema), dialect.quote(name)),
                    [],
                    |row| row.get(3),
                )
                .unwrap();
            assert!(descending, "{sql}");
        }
    }
}
