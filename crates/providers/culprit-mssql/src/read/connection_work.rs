//! Observe only the original owner future. No timeout, native dispatch, retry or settlement is introduced here.
use super::*;
use culprit_core::telemetry::{ReadConnectionAttemptRecorder, ReadConnectionOutcome, ReadConnectionStage};
use std::future::Future;
pub(crate) async fn core_phase<T>(
    attempt: Option<&ReadConnectionAttemptRecorder>,
    stage: ReadConnectionStage,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    let phase = attempt.map(|attempt| attempt.phase(stage, None));
    let result = future.await;
    if let Some(phase) = phase {
        phase.finish(match &result {
            Ok(_) => ReadConnectionOutcome::Completed,
            Err(error) => ReadConnectionOutcome::Failed { code: error.code, native_code: None },
        });
    }
    result
}
pub(crate) async fn native_phase<T>(
    attempt: Option<&ReadConnectionAttemptRecorder>,
    stage: ReadConnectionStage,
    future: impl Future<Output = tiberius::Result<T>>,
) -> tiberius::Result<T> {
    let phase = attempt.map(|attempt| attempt.phase(stage, None));
    let result = future.await;
    if let Some(phase) = phase {
        phase.finish(match &result {
            Ok(_) => ReadConnectionOutcome::Completed,
            Err(tiberius::error::Error::Routing { .. }) if stage == ReadConnectionStage::HandshakeAndLogin => {
                ReadConnectionOutcome::Redirected
            }
            Err(error) => ReadConnectionOutcome::Failed {
                code: native_error_code(error),
                native_code: match error {
                    tiberius::error::Error::Server(token) => Some(token.code()),
                    _ => None,
                },
            },
        });
    }
    result
}
fn native_error_code(error: &tiberius::error::Error) -> culprit_core::ErrorCode {
    use culprit_core::ErrorCode;
    use tiberius::error::Error as Native;
    match error {
        Native::Server(token) => crate::conn::server_error(token.code(), String::new()).code,
        Native::Io { kind, .. } if *kind == std::io::ErrorKind::TimedOut => ErrorCode::Timeout,
        Native::Io { .. } | Native::Tls(_) | Native::Routing { .. } => ErrorCode::ConnectionFailed,
        _ => ErrorCode::Internal,
    }
}
pub(crate) fn outcome<T>(result: &ReadResult<T>) -> ReadConnectionOutcome {
    match result {
        Ok(_) => ReadConnectionOutcome::Completed,
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)) => ReadConnectionOutcome::Unconfirmed,
        Err(ReadError::Stopped(reason)) => ReadConnectionOutcome::Stopped { reason: *reason },
        Err(ReadError::Source(error)) => ReadConnectionOutcome::Failed { code: error.code, native_code: None },
    }
}
#[cfg(test)]
mod tests;
