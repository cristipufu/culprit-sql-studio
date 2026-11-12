//! KILL acknowledges the control command, not completion of the original request.
use crate::rows::tagged;
use culprit_core::telemetry::SourceShutdown;
use mysql_async::{Conn, prelude::Queryable};
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// This describes the control connection only. Even Settled never proves the target query stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlCompletion {
    Settled,
    Unconfirmed,
}
impl ControlCompletion {
    fn from_response<T>(result: &mysql_async::Result<T>) -> Self {
        if response_completed(result) { Self::Settled } else { Self::Unconfirmed }
    }
}

/// Wait for the original response and every control command that was dispatched. An interrupted connection must
/// then be discarded: no delayed cancellation may reach its next statement.
pub(crate) async fn settle<T>(
    response: impl Future<Output = mysql_async::Result<T>>,
    cancellation: impl Future<Output = ControlCompletion>,
    timeout: Duration,
) -> SourceShutdown {
    tokio::pin!(response, cancellation);
    let control_started = AtomicBool::new(false);
    let control = std::future::poll_fn(|cx| {
        control_started.store(true, Ordering::Release);
        cancellation.as_mut().poll(cx)
    });
    tokio::pin!(control);
    let confirmed = tokio::time::timeout(timeout, async {
        tokio::select! {
            biased;
            result = &mut response => {
                let original_complete = response_completed(&result);
                let control_complete = !control_started.load(Ordering::Acquire)
                    || control.await == ControlCompletion::Settled;
                original_complete && control_complete
            }
            control_complete = &mut control => {
                response_completed(&response.await) && control_complete == ControlCompletion::Settled
            }
        }
    })
    .await
    .unwrap_or(false);
    if confirmed { SourceShutdown::Quiescent } else { SourceShutdown::Unconfirmed }
}

pub(crate) fn response_completed<T>(result: &mysql_async::Result<T>) -> bool {
    matches!(
        result,
        Ok(_) | Err(mysql_async::Error::Server(_))
        // mysql_async validates these EXECUTE arguments before dispatch. PREPARE is a separate completed phase.
        | Err(mysql_async::Error::Driver(mysql_async::DriverError::StmtParamsMismatch { .. }))
    )
}

/// The marker identifies our exact in-flight request, including behind a server-local connection ID. Keep the
/// original connection owned until this control operation settles; never reconnect/reuse it during cancellation.
/// https://dev.mysql.com/doc/refman/8.4/en/kill.html documents that a successful KILL does not await its target.
pub(crate) async fn request(
    control: &mut Conn,
    id: u32,
    marker: &str,
    accounting: &crate::read::accounting::Accounting,
) -> ControlCompletion {
    let measurement = accounting.control_query("SELECT LEFT(INFO, ?) FROM information_schema.PROCESSLIST WHERE ID = ?");
    // IDs come from the owned native client, not SQL text. Fetch only our bounded marker prefix, never the full
    // running statement. This command needs no hidden PREPARE/EXECUTE pair during cancellation.
    let current: mysql_async::Result<Option<(Option<String>,)>> = control
        .query_first(tagged(&format!(
            "SELECT LEFT(INFO,{}) FROM information_schema.PROCESSLIST WHERE ID={id}",
            marker.len()
        )))
        .await;
    let rows = u64::from(current.as_ref().is_ok_and(Option::is_some));
    let bytes = current
        .as_ref()
        .ok()
        .and_then(|row| row.as_ref())
        .and_then(|(sql,)| sql.as_ref())
        .map_or(0, |sql| sql.len() as u64);
    measurement.complete(&current, rows, bytes);
    match current {
        Ok(Some((Some(sql),))) if sql.starts_with(marker) => {
            let measurement = accounting.control_query("KILL QUERY ?");
            let result = control.query_drop(tagged(&format!("KILL QUERY {id}"))).await;
            measurement.complete(&result, 0, 0);
            ControlCompletion::from_response(&result)
        }
        Ok(_) | Err(mysql_async::Error::Server(_)) => ControlCompletion::Settled,
        Err(_) => ControlCompletion::Unconfirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{pending, ready},
        sync::Arc,
    };
    const GRACE: Duration = Duration::from_millis(30);

    #[tokio::test(start_paused = true)]
    async fn accepting_kill_never_confirms_the_original_request_finished() {
        assert_eq!(
            settle(pending::<mysql_async::Result<()>>(), ready(ControlCompletion::Settled), GRACE).await,
            SourceShutdown::Unconfirmed
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_transport_error_is_unconfirmed_even_after_successful_kill() {
        let response = ready(Err::<(), _>(std::io::Error::other("connection lost").into()));
        assert_eq!(settle(response, ready(ControlCompletion::Settled), GRACE).await, SourceShutdown::Unconfirmed);
    }
    #[tokio::test(start_paused = true)]
    async fn native_completion_waits_for_the_original_reply_after_kill() {
        let response = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Err::<(), _>(mysql_async::Error::Server(mysql_async::ServerError {
                code: 1317,
                message: "interrupted".into(),
                state: "70100".into(),
            }))
        };
        assert_eq!(settle(response, ready(ControlCompletion::Settled), GRACE).await, SourceShutdown::Quiescent);
    }
    #[tokio::test(start_paused = true)]
    async fn a_completed_request_does_not_dispatch_a_fresh_control_command() {
        let started = Arc::new(AtomicBool::new(false));
        let observe = started.clone();
        let cancellation = async move {
            observe.store(true, Ordering::Release);
            ControlCompletion::Settled
        };
        assert_eq!(
            settle(ready(Ok::<_, mysql_async::Error>(())), cancellation, GRACE).await,
            SourceShutdown::Quiescent
        );
        assert!(!started.load(Ordering::Acquire));
    }
    #[tokio::test(start_paused = true)]
    async fn an_already_dispatched_control_command_must_also_settle() {
        let response = async {
            tokio::task::yield_now().await;
            Ok::<_, mysql_async::Error>(())
        };
        assert_eq!(settle(response, pending(), GRACE).await, SourceShutdown::Unconfirmed);
        let response = async {
            tokio::task::yield_now().await;
            Ok::<_, mysql_async::Error>(())
        };
        assert_eq!(settle(response, ready(ControlCompletion::Unconfirmed), GRACE).await, SourceShutdown::Unconfirmed);
    }
}
