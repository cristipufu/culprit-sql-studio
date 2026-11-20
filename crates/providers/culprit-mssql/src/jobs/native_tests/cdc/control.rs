//! A drained fixture command and physical socket retirement are separate lifecycle facts.
use crate::{conn::map_error, read::invalid_data};
use culprit_core::{Error, Result, telemetry::CollectionStop};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

use super::CONTROL_TIMEOUT;

type ControlResult<T> = std::result::Result<T, ControlFailure>;

pub(super) struct ControlFailure {
    error: Error,
    native_error_number: Option<u32>,
}
impl From<Error> for ControlFailure {
    fn from(error: Error) -> Self {
        // Native error text can contain SQL or a failing row value. Retain only typed facts.
        let mut safe = invalid_data();
        safe.code = error.code;
        safe.detail = error.detail;
        Self { error: safe, native_error_number: None }
    }
}
impl From<tiberius::error::Error> for ControlFailure {
    fn from(error: tiberius::error::Error) -> Self {
        let native_error_number = error.code();
        Self { native_error_number, ..Self::from(map_error(error)) }
    }
}

pub(super) struct Control {
    available: bool,
    events: Vec<Value>,
}
impl Control {
    pub fn new() -> Self {
        Self { available: true, events: Vec::new() }
    }
    /// An unfinished multi-command fixture phase cannot be repaired by a successful socket close.
    pub fn fence(&mut self) {
        self.available = false;
    }
    pub fn is_available(&self) -> bool {
        self.available
    }
    pub fn require_available(&self) -> Result<()> {
        if self.available { Ok(()) } else { Err(CollectionStop::CancellationUnconfirmed.error()) }
    }
    pub fn events(&self) -> &[Value] {
        &self.events
    }
    pub async fn run<T>(&mut self, stage: &'static str, work: impl Future<Output = ControlResult<T>>) -> Result<T> {
        Box::pin(self.run_with_timeout(stage, CONTROL_TIMEOUT, work)).await
    }
    pub async fn run_with_timeout<T>(
        &mut self,
        stage: &'static str,
        timeout: Duration,
        work: impl Future<Output = ControlResult<T>>,
    ) -> Result<T> {
        self.require_available()?;
        // Remains closed on timeout, error, panic or abandonment. A successful close cannot reset it.
        self.available = false;
        let started = Instant::now();
        let position = self.start(stage, "native-operation", timeout);
        let result = Box::pin(bounded(timeout, work)).await;
        self.available = result.is_ok();
        self.finish(position, started, &result);
        result.map_err(|failure| failure.error)
    }
    pub async fn retire(&mut self, work: impl Future<Output = tiberius::Result<()>>) -> Result<()> {
        let previously_available = self.available;
        self.available = false;
        let started = Instant::now();
        let position = self.start("retire-control", "physical-close", CONTROL_TIMEOUT);
        let result = Box::pin(bounded(CONTROL_TIMEOUT, async { work.await.map_err(ControlFailure::from) })).await;
        self.available = previously_available && result.is_ok();
        self.finish(position, started, &result);
        result.map_err(|failure| failure.error)
    }
    fn start(&mut self, stage: &'static str, kind: &'static str, timeout: Duration) -> usize {
        let position = self.events.len();
        self.events
            .push(json!({"stage":stage,"kind":kind,"allowed_ms":timeout.as_secs_f64()*1000.0,"outcome":"in-progress"}));
        position
    }
    fn finish<T>(&mut self, position: usize, started: Instant, result: &ControlResult<T>) {
        let event = &mut self.events[position];
        event["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        event["outcome"] = json!(if result.is_ok() { "completed" } else { "failed" });
        event["further_fixture_sql_allowed"] = json!(self.available);
        if let Err(failure) = result {
            event["error_code"] = json!(failure.error.code);
            event["error_detail"] = json!(failure.error.detail);
            event["native_error_number"] = json!(failure.native_error_number);
        }
    }
}
async fn bounded<T>(timeout: Duration, work: impl Future<Output = ControlResult<T>>) -> ControlResult<T> {
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| ControlFailure::from(CollectionStop::CancellationUnconfirmed.error()))?
}

#[cfg(test)]
mod tests {
    use super::super::{FIRST_TABLE_SETUP_TIMEOUT, control_config};
    use super::*;
    use culprit_core::{ErrorCode, ErrorDetail};
    use futures_util::FutureExt;
    use std::{
        panic::AssertUnwindSafe,
        sync::atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test(start_paused = true)]
    async fn explicit_first_table_setup_allowance_does_not_extend_ordinary_control_work() {
        let mut control = Control::new();
        control
            .run_with_timeout("enable-table", FIRST_TABLE_SETUP_TIMEOUT, async {
                tokio::time::sleep(CONTROL_TIMEOUT + Duration::from_secs(1)).await;
                Ok(())
            })
            .await
            .unwrap();
        assert!(control.is_available(), "the explicit setup allowance must permit a drained response after 5s");
        let started = Instant::now();
        let ordinary = control.run("stop-capture-job", std::future::pending::<ControlResult<()>>()).await.unwrap_err();
        assert_eq!(started.elapsed(), CONTROL_TIMEOUT, "ordinary control still stops at its original 5s deadline");
        assert_eq!(ordinary.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)));
        assert_eq!(control.events()[0]["allowed_ms"], FIRST_TABLE_SETUP_TIMEOUT.as_secs_f64() * 1000.0);
        assert_eq!(control.events()[1]["allowed_ms"], CONTROL_TIMEOUT.as_secs_f64() * 1000.0);
    }

    #[tokio::test(start_paused = true)]
    async fn first_table_setup_still_has_a_finite_deadline_and_close_cannot_reopen_it() {
        let mut control = Control::new();
        let started = Instant::now();
        let error = control
            .run_with_timeout("enable-table", FIRST_TABLE_SETUP_TIMEOUT, std::future::pending::<ControlResult<()>>())
            .await
            .unwrap_err();
        assert_eq!(started.elapsed(), FIRST_TABLE_SETUP_TIMEOUT);
        assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)));
        control.retire(async { Ok(()) }).await.unwrap();
        let calls = AtomicUsize::new(0);
        assert!(
            control
                .run("cleanup-connect", async {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(!control.is_available());
    }

    #[test]
    fn fixture_protocol_response_ceiling_permits_the_explicit_first_table_setup_deadline() {
        let options = control_config(0, "unused", "unused");
        assert_eq!(options.get_command_timeout(), Some(FIRST_TABLE_SETUP_TIMEOUT));
        assert!(CONTROL_TIMEOUT < FIRST_TABLE_SETUP_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_native_operation_stays_fenced_after_successful_physical_close() {
        let mut control = Control::new();
        let error = control.run("enable-table", std::future::pending::<ControlResult<()>>()).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)));
        control.retire(async { Ok(()) }).await.unwrap();
        let calls = AtomicUsize::new(0);
        for stage in ["cleanup-connect", "disable-cdc", "drop-table"] {
            assert!(
                control
                    .run(stage, async {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    })
                    .await
                    .is_err()
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0, "cleanup must not start replacement SQL");
        assert_eq!(control.events()[0]["stage"], "enable-table");
        assert_eq!(control.events()[0]["elapsed_ms"], CONTROL_TIMEOUT.as_secs_f64() * 1000.0);
        assert_eq!(control.events()[1]["outcome"], "completed");
        assert!(!control.is_available(), "physical close does not establish native command completion");
    }

    #[tokio::test]
    async fn a_panicking_original_command_cannot_reopen_admission_through_cleanup() {
        let mut control = Control::new();
        let result = AssertUnwindSafe(control.run::<()>("stop-capture-job", async { panic!("fixture panic") }))
            .catch_unwind()
            .await;
        assert!(result.is_err());
        control.retire(async { Ok(()) }).await.unwrap();
        assert!(!control.is_available());
        assert_eq!(control.events()[0]["outcome"], "in-progress");
        assert!(control.require_available().is_err());
    }

    #[tokio::test]
    async fn safe_stage_errors_keep_numeric_native_evidence_and_the_original_failure() {
        let mut control = Control::new();
        let private = "SELECT private_sql; failing row owned-cdc-private-value";
        let failure = ControlFailure {
            native_error_number: Some(229),
            ..ControlFailure::from(Error::new(ErrorCode::PermissionDenied, private).with_hint(private))
        };
        let error = control.run("enable-table", async { Err::<(), _>(failure) }).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert!(!error.message.contains(private) && error.hint.is_none());
        let retirement =
            control.retire(async { Err(tiberius::error::Error::Protocol("private retirement text".into())) }).await;
        assert!(retirement.is_err());
        let receipt = serde_json::to_string(control.events()).unwrap();
        assert!(!receipt.contains(private) && !receipt.contains("private retirement text"));
        assert_eq!(control.events()[0]["native_error_number"], 229);
        assert_eq!(control.events()[0]["error_code"], "permission_denied");
        assert_eq!(error.code, ErrorCode::PermissionDenied, "retirement cannot replace the first failure");
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_or_failed_retirement_prevents_a_replacement_control_connection() {
        for timed_out in [false, true] {
            let mut control = Control::new();
            control.run("create-table", async { Ok(()) }).await.unwrap();
            let error = control
                .retire(async {
                    if timed_out {
                        std::future::pending::<tiberius::Result<()>>().await
                    } else {
                        Err(tiberius::error::Error::Protocol("retirement failure".into()))
                    }
                })
                .await;
            assert!(error.is_err());
            assert!(control.require_available().is_err());
        }
    }

    #[tokio::test]
    async fn an_unfinished_phase_refusal_stays_fenced_after_physical_retirement() {
        let mut control = Control::new();
        control.run("observe-owned-runtime", async { Ok(()) }).await.unwrap();
        control.fence();
        control.retire(async { Ok(()) }).await.unwrap();
        let calls = AtomicUsize::new(0);
        assert!(
            control
                .run("cleanup-connect", async {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(!control.is_available());
    }

    #[tokio::test]
    async fn a_native_stop_error_including_not_running_is_always_failure_and_permanent_fencing() {
        for number in [22022, 229, 14262] {
            let mut control = Control::new();
            let failure = ControlFailure {
                native_error_number: Some(number),
                ..ControlFailure::from(Error::new(ErrorCode::QueryFailed, "private native error"))
            };
            let error =
                control.run("stop-executing-owned-capture-job", async { Err::<(), _>(failure) }).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::QueryFailed);
            assert_eq!(control.events()[0]["native_error_number"], number);
            control.retire(async { Ok(()) }).await.unwrap();
            assert!(control.require_available().is_err());
            assert!(!serde_json::to_string(control.events()).unwrap().contains("private native error"));
        }
    }

    #[tokio::test]
    async fn named_drained_operations_preserve_their_individual_outcomes() {
        let mut control = Control::new();
        for stage in ["create-table", "seed-table", "enable-table", "stop-capture-job", "insert-positive"] {
            control.run(stage, async { Ok(()) }).await.unwrap();
            assert!(control.is_available());
        }
        control.retire(async { Ok(()) }).await.unwrap();
        assert!(control.is_available());
        assert_eq!(control.events().len(), 6);
        assert!(control.events().iter().all(|event| event["outcome"] == "completed"));
        assert_eq!(control.events()[2]["stage"], "enable-table");
        assert_eq!(control.events()[3]["stage"], "stop-capture-job");
    }
}
