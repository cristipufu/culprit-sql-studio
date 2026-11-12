//! The display policy owns its concurrency slot; all native setup, query and cancellation belong to the common reader.
use crate::connect::Connector;
use culprit_core::{Error, Result, ResultSet, t};
use std::sync::Arc;
use tokio::sync::Semaphore;

// Include retiring workers so navigation cannot accumulate native requests behind the UI gate.
static PREVIEWS: Semaphore = Semaphore::const_new(2);
pub(crate) async fn read(connector: Arc<Connector>, sql: String, limit: usize) -> Result<ResultSet> {
    let permit = PREVIEWS.acquire().await.map_err(|_| Error::internal(t!("provider.mysql.preview.gate_closed")))?;
    crate::read::preview(connector, sql, limit, permit).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cancellation,
        connect::{Scope, TAG},
        rows::tagged,
    };
    use culprit_core::{limits::PREVIEW_TIMEOUT, telemetry::SourceShutdown};
    use mysql_async::{Conn, prelude::Queryable};
    use std::time::{Duration, Instant};
    use tokio::sync::oneshot;
    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
    fn marker() -> String {
        format!("{TAG} /* unmatched-preview-fixture */")
    }
    async fn disconnect(conn: Conn) {
        let _ = tokio::time::timeout(CLEANUP_TIMEOUT, conn.disconnect()).await;
    }
    async fn worker(
        client: Conn,
        control: Conn,
        sql: &str,
        limit: usize,
        setting: &str,
        cancellation: oneshot::Receiver<()>,
    ) -> Result<ResultSet> {
        crate::read::preview_with_clients(connector(), client, control, sql, limit, setting, cancellation).await
    }
    use culprit_core::{ConnectionSpec, Engine, ErrorCode};

    #[tokio::test]
    async fn a_late_native_timeout_confirms_completion_after_an_idle_process_list() {
        let response = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Err::<(), _>(mysql_async::Error::Server(mysql_async::ServerError {
                code: 3024,
                message: "execution timeout".into(),
                state: "HY000".into(),
            }))
        };
        assert_eq!(
            cancellation::settle(
                response,
                std::future::ready(cancellation::ControlCompletion::Settled),
                CLEANUP_TIMEOUT
            )
            .await,
            SourceShutdown::Quiescent
        );
        assert_eq!(
            cancellation::settle(
                std::future::ready(Ok::<_, mysql_async::Error>(())),
                std::future::pending(),
                CLEANUP_TIMEOUT
            )
            .await,
            SourceShutdown::Quiescent
        );
    }

    fn connector() -> Arc<Connector> {
        Arc::new(
            Connector::new(&ConnectionSpec {
                engine: Some(Engine::Mysql),
                host: "127.0.0.1".into(),
                port: Some(
                    std::env::var("CULPRIT_MYSQL_TEST_PORT").expect("set the disposable MySQL port").parse().unwrap(),
                ),
                user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or("root".into())),
                password: Some(
                    std::env::var("CULPRIT_MYSQL_TEST_PASSWORD").expect("set the disposable MySQL password"),
                ),
                ..Default::default()
            })
            .unwrap(),
        )
    }

    async fn active(connector: &Connector, id: u32) -> u64 {
        let mut conn = connector.connect(Scope::Server).await.unwrap();
        let count = conn.exec_first::<u64, _, _>(
            tagged("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ? AND INFO LIKE '%culprit_preview_cancel_test%'"), (id,),
        ).await.unwrap().unwrap();
        disconnect(conn).await;
        count
    }

    #[tokio::test]
    #[ignore = "needs a disposable MySQL/MariaDB server and CULPRIT_MYSQL_TEST_PORT/PASSWORD"]
    async fn live_query_timeout_excludes_slow_setup_without_a_spurious_warning() {
        let connector = connector();
        let mut conn = connector.connect(Scope::Database).await.unwrap();
        conn.query_drop(tagged(&connector.flavor().timeout_for(PREVIEW_TIMEOUT))).await.unwrap();
        let control = connector.connect(Scope::Server).await.unwrap();
        let (owner, canceled) = oneshot::channel();
        // A deliberately slow optional setup round-trip used to shorten the SELECT's client budget by 500 ms.
        let started = Instant::now();
        // Stream a large Cartesian product of tiny derived tables: no fixture data or SLEEP shortcut in the query.
        let digits = "(SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)";
        let tables = (0..9).map(|i| format!("{digits} AS n{i}")).collect::<Vec<_>>().join(" CROSS JOIN ");
        let sum = (0..9).map(|i| format!("n{i}.n")).collect::<Vec<_>>().join(" + ");
        let sql = format!("SELECT COUNT(*) AS culprit_preview_cancel_test FROM {tables} WHERE MOD({sum}, 2) = 0");
        let error = worker(conn, control, &sql, 1, "SELECT SLEEP(0.5)", canceled).await.unwrap_err();
        drop(owner);
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(!error.message.contains("could not confirm"), "{}", error.message);
        assert!(started.elapsed() >= PREVIEW_TIMEOUT + Duration::from_millis(400), "{:?}", started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    }

    #[tokio::test]
    #[ignore = "needs a disposable MySQL/MariaDB server and CULPRIT_MYSQL_TEST_PORT/PASSWORD"]
    async fn live_deadline_and_dropped_callers_cancel_without_a_server_timeout() {
        let connector = connector();
        let sql = "SELECT SLEEP(30) AS culprit_preview_cancel_test";
        let conn = connector.connect(Scope::Database).await.unwrap();
        let control = connector.connect(Scope::Server).await.unwrap();
        let (owner, canceled) = oneshot::channel();
        // An unsupported setting must not prevent ordinary previews.
        let result = worker(conn, control, "SELECT 42", 1, "SET SESSION culprit_unsupported_timeout = 5", canceled)
            .await
            .unwrap();
        assert_eq!(result.rows[0][0], serde_json::json!(42));
        drop(owner);
        let mut conn = connector.connect(Scope::Database).await.unwrap();
        // Explicitly disable the server timer; cancellation must come from the worker's KILL QUERY.
        let disable = match connector.flavor() {
            crate::connect::Flavor::Mysql => "SET SESSION max_execution_time = 0",
            crate::connect::Flavor::Mariadb => "SET SESSION max_statement_time = 0",
        };
        conn.query_drop(tagged(disable)).await.unwrap();
        let control = connector.connect(Scope::Server).await.unwrap();
        let (owner, canceled) = oneshot::channel();
        let id = conn.id();
        let started = Instant::now();
        let error =
            worker(conn, control, sql, 1, "SET SESSION culprit_unsupported_timeout = 5", canceled).await.unwrap_err();
        drop(owner);
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        assert_eq!(active(&connector, id).await, 0, "deadline left a server query running");

        let mut conn = connector.connect(Scope::Database).await.unwrap();
        conn.query_drop(tagged(disable)).await.unwrap();
        let control = connector.connect(Scope::Server).await.unwrap();
        let (owner, canceled) = oneshot::channel();
        let id = conn.id();
        let task = tokio::spawn(async move {
            worker(conn, control, sql, 1, "SET SESSION culprit_unsupported_timeout = 5", canceled).await
        });
        for _ in 0..100 {
            if active(&connector, id).await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(active(&connector, id).await, 1, "query never became active");
        drop(owner);
        assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::Timeout);
        assert_eq!(active(&connector, id).await, 0, "caller cancellation left a server query running");
        assert_eq!(read(connector, "SELECT 42".into(), 1).await.unwrap().rows[0][0], serde_json::json!(42));
    }

    #[tokio::test]
    #[ignore = "needs a disposable MySQL/MariaDB server and CULPRIT_MYSQL_TEST_PORT/PASSWORD"]
    async fn live_cancellation_never_kills_a_different_tag() {
        let connector = connector();
        let mut conn = connector.connect(Scope::Database).await.unwrap();
        let mut control = connector.connect(Scope::Server).await.unwrap();
        let id = conn.id();
        let task = tokio::spawn(async move {
            let result = conn.query_drop(tagged("SELECT SLEEP(30) AS culprit_preview_cancel_test")).await;
            disconnect(conn).await;
            result
        });
        for _ in 0..100 {
            if active(&connector, id).await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            cancellation::request(
                &mut control,
                id,
                &marker(),
                &crate::read::accounting::Accounting::new(Default::default())
            )
            .await,
            cancellation::ControlCompletion::Settled,
            "control lookup did not settle"
        );
        assert_eq!(active(&connector, id).await, 1, "different-tag cancellation stopped the query");
        control.query_drop(tagged(&format!("KILL QUERY {id}"))).await.unwrap();
        let _ = task.await.unwrap();
        disconnect(control).await;
    }
}
