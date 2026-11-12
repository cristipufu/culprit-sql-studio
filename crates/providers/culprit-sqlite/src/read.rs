//! One owner retains each blocking SQLite call through statement finalization and connection close.
mod accounting;
use crate::{map_error, task_stopped};
pub(crate) use accounting::FileAccounting;
use culprit_core::{
    Result,
    telemetry::{CollectionStop, SourceShutdown},
};
use culprit_read::ReadFence;
use rusqlite::{Connection, OpenFlags};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const BUSY_WAIT: Duration = Duration::from_millis(20);
static STATEMENTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const CLOSE_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Default)]
struct Interruption(Arc<Mutex<Option<rusqlite::InterruptHandle>>>);
impl Interruption {
    fn interrupt(&self) {
        if let Some(handle) = self.0.lock().expect("SQLite interruption owner").as_ref() {
            handle.interrupt();
        }
    }
}
struct CancelOnDrop(Interruption);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.interrupt();
    }
}
#[derive(Clone)]
pub(crate) struct FileReader {
    path: PathBuf,
    fence: ReadFence,
    worker: Arc<tokio::sync::Semaphore>,
}
impl FileReader {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, fence: ReadFence::default(), worker: Arc::new(tokio::sync::Semaphore::new(1)) }
    }
    pub(crate) fn shutdown(&self) -> SourceShutdown {
        if self.fence.is_uncertain() { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent }
    }
    pub(crate) async fn execute<T: Send + 'static>(
        &self,
        budget: Duration,
        operation: impl FnOnce(&mut Connection, &std::path::Path) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.with_handle(budget, move |conn, path, _| operation(conn, path)).await
    }
    pub(crate) async fn with_handle<T: Send + 'static>(
        &self,
        budget: Duration,
        operation: impl FnOnce(&mut Connection, &std::path::Path, culprit_core::limits::OperationHandle) -> Result<T>
        + Send
        + 'static,
    ) -> Result<T> {
        self.owned(budget, FileAccounting::new(u32::MAX), move |conn, path, handle, _| operation(conn, path, handle))
            .await
    }
    pub(crate) async fn with_accounting<T: Send + 'static>(
        &self,
        budget: Duration,
        accounting: FileAccounting,
        operation: impl FnOnce(&mut Connection, &std::path::Path, FileAccounting) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.owned(budget, accounting, move |conn, path, _, accounting| operation(conn, path, accounting)).await
    }
    async fn owned<T: Send + 'static>(
        &self,
        budget: Duration,
        accounting: FileAccounting,
        operation: impl FnOnce(
            &mut Connection,
            &std::path::Path,
            culprit_core::limits::OperationHandle,
            FileAccounting,
        ) -> Result<T>
        + Send
        + 'static,
    ) -> Result<T> {
        // The caller guard fences immediately on abandonment; a later worker completion cannot erase that debt.

        let guard = culprit_core::limits::OperationGuard::new(budget)?;
        let handle = guard.handle();
        let permit = tokio::time::timeout(handle.remaining(), self.worker.clone().acquire_owned())
            .await
            .map_err(|_| CollectionStop::TimedOut.error())?
            .map_err(|_| crate::execution_unavailable())?;
        let capacity = tokio::time::timeout(handle.remaining(), STATEMENTS.acquire())
            .await
            .map_err(|_| CollectionStop::TimedOut.error())?
            .map_err(|_| crate::execution_unavailable())?;
        let caller = self.fence.start_work()?;
        let worker = self.fence.start_work()?;
        let interruption = Interruption::default();
        let cancellation = CancelOnDrop(interruption.clone());
        let path = self.path.clone();
        let remaining = handle.remaining();
        let mut task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _capacity = capacity;
            let (result, shutdown) = run(&path, handle, interruption, accounting, operation);
            worker.complete(shutdown);
            result
        });
        let result = match tokio::time::timeout(remaining, &mut task).await {
            Ok(result) => result.map_err(|error| task_stopped(&error)),
            Err(_) => {
                cancellation.0.interrupt();
                drop(guard);
                match tokio::time::timeout(CLOSE_GRACE, &mut task).await {
                    Ok(Ok(_)) if self.shutdown() == SourceShutdown::Quiescent => {
                        caller.complete(SourceShutdown::Quiescent);
                        return Err(CollectionStop::TimedOut.error());
                    }
                    _ => return Err(CollectionStop::CancellationUnconfirmed.error()),
                }
            }
        };
        if result.is_ok() {
            caller.complete(SourceShutdown::Quiescent);
        }
        result?
    }
}

pub(crate) fn open(path: &std::path::Path) -> Result<Connection> {
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(culprit_core::Error::new(
                culprit_core::ErrorCode::NotFound,
                culprit_core::t!("provider.sqlite.open.no_file", path = path.display().to_string()),
            )
            .with_hint(culprit_core::t!("provider.sqlite.open.no_file.hint")));
        }
        Err(error) => {
            return Err(culprit_core::Error::new(culprit_core::ErrorCode::ConnectionFailed, error.to_string()));
        }
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| map_error(&error))
}

fn run<T>(
    path: &std::path::Path,
    handle: culprit_core::limits::OperationHandle,
    interruption: Interruption,
    accounting: FileAccounting,
    operation: impl FnOnce(
        &mut Connection,
        &std::path::Path,
        culprit_core::limits::OperationHandle,
        FileAccounting,
    ) -> Result<T>,
) -> (Result<T>, SourceShutdown) {
    if let Err(error) = handle.check() {
        return (Err(error), SourceShutdown::Quiescent);
    }
    let mut conn = match open(path) {
        Ok(conn) => conn,
        Err(error) => return (Err(error), SourceShutdown::Quiescent),
    };
    *interruption.0.lock().expect("SQLite interruption owner") = Some(conn.get_interrupt_handle());
    let result = (|| {
        for limit in [rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH] {
            conn.set_limit(limit, culprit_core::limits::MAX_RESULT_BYTES as i32).map_err(|error| map_error(&error))?;
        }
        let progress = handle.clone();
        conn.progress_handler(100, Some(move || progress.stopped())).map_err(|error| map_error(&error))?;
        conn.busy_timeout(BUSY_WAIT).map_err(|error| map_error(&error))?;
        for sql in ["/* culprit */ PRAGMA query_only=1", "/* culprit */ PRAGMA trusted_schema=OFF"] {
            accounting
                .statement(sql, &Default::default(), || conn.execute_batch(sql).map_err(|error| map_error(&error)))?;
        }
        handle.check()?;
        operation(&mut conn, path, handle, accounting)
    })();
    let result = match conn.close() {
        Ok(()) => (result, SourceShutdown::Quiescent),
        Err((_conn, _error)) => (Err(CollectionStop::CancellationUnconfirmed.error()), SourceShutdown::Unconfirmed),
    };
    *interruption.0.lock().expect("SQLite interruption owner") = None;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "culprit-file-owner-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        drop(Connection::open(&path).unwrap());
        path
    }
    #[tokio::test]
    async fn queued_deadline_starts_no_sql_and_does_not_fence_the_owner() {
        let path = fixture();
        let reader = FileReader::new(path.clone());
        let permit = reader.worker.acquire().await.unwrap();
        let result = reader
            .execute(Duration::from_millis(10), |_, _| -> Result<()> { panic!("queued expired work cannot start") })
            .await;
        assert!(result.is_err());
        assert_eq!(reader.shutdown(), SourceShutdown::Quiescent);
        drop(permit);
        reader
            .execute(Duration::from_secs(1), |conn, _| {
                conn.query_row("/* culprit */ SELECT 1", [], |row| row.get::<_, u32>(0))
                    .map_err(|error| map_error(&error))
            })
            .await
            .unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn abandoning_a_worker_fences_all_provider_readers_until_its_file_is_closed() {
        let path = fixture();
        let reader = FileReader::new(path.clone());
        let worker = reader.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            worker.execute(Duration::from_secs(5),move |conn,_| {
            let _=started.send(());
            conn.query_row("/* culprit */ WITH RECURSIVE c(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM c WHERE x<10000000) SELECT sum(x) FROM c",[],|row|row.get::<_,i64>(0)).map_err(|error|map_error(&error))
        }).await
        });
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        let permit = tokio::time::timeout(Duration::from_secs(2), reader.worker.acquire()).await.unwrap().unwrap();
        assert_eq!(reader.shutdown(), SourceShutdown::Unconfirmed);
        drop(permit);
        assert!(reader.execute(Duration::from_secs(1), |_, _| Ok(())).await.is_err());
        std::fs::remove_file(path).unwrap();
    }
}
