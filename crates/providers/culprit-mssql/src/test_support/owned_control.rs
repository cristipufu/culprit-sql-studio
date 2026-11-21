//! Existing nonce-fixture command retirement policy, shared without changing its native meaning.
use futures_util::FutureExt;
use std::{
    panic::{AssertUnwindSafe, resume_unwind},
    time::Duration,
};
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) type BoundedControl<T> = std::thread::Result<std::result::Result<T, tokio::time::error::Elapsed>>;

/// A drained result and socket retirement are separate facts. Only both succeeding reopen this fixture gate.
pub(crate) struct RetiredControl<T> {
    operation: BoundedControl<T>,
    retirement: BoundedControl<tiberius::Result<()>>,
}
pub(crate) async fn retire_control<T>(
    operation: BoundedControl<T>,
    retirement: impl std::future::Future<Output = tiberius::Result<()>>,
) -> RetiredControl<T> {
    let retirement = AssertUnwindSafe(tokio::time::timeout(COMMAND_TIMEOUT, retirement)).catch_unwind().await;
    RetiredControl { operation, retirement }
}
impl<T> RetiredControl<T> {
    pub(crate) fn permits_sql(&self) -> bool {
        matches!(self.operation, Ok(Ok(_))) && matches!(self.retirement, Ok(Ok(Ok(()))))
    }
    pub(crate) fn finish(self) -> T {
        // Retirement was already awaited. Its failure can never replace the original operation failure.
        let value = match self.operation {
            Err(panic) => resume_unwind(panic),
            Ok(Err(timeout)) => panic!("bounded owned catalog control timed out: {timeout}"),
            Ok(Ok(value)) => value,
        };
        match self.retirement {
            Err(panic) => resume_unwind(panic),
            Ok(Err(timeout)) => panic!("bounded original fixture client retirement timed out: {timeout}"),
            Ok(Ok(result)) => result.expect("original fixture client retirement"),
        }
        value
    }
}
