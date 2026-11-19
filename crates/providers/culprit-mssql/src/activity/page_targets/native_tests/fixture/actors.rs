//! One retained actor lifecycle for native workloads with different, honestly typed completions.
use super::invalid;
use crate::{
    conn::{MsClient, map_error},
    value::Rec,
};
use culprit_core::Result;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Barrier, task::JoinHandle};

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const WORKER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
type Slots<T> = Vec<Option<JoinHandle<Result<T>>>>;

pub(in crate::activity::page_targets::native_tests) async fn start<K: Send + 'static, T: Send + 'static>(
    slots: &mut Slots<T>,
    clients: Vec<(MsClient, K, String)>,
    completed: fn(K, &Rec) -> Result<T>,
) -> Result<Instant> {
    if !slots.is_empty() || clients.is_empty() {
        return Err(invalid("owned actors require one nonempty fresh population"));
    }
    let barrier = Arc::new(Barrier::new(clients.len() + 1));
    for (mut client, key, sql) in clients {
        let barrier = barrier.clone();
        slots.push(Some(tokio::spawn(async move {
            barrier.wait().await;
            let rows =
                client.simple_query(sql).await.map_err(map_error)?.into_first_result().await.map_err(map_error)?;
            let row = rows.first().map(Rec::from_row).ok_or_else(|| invalid("owned worker returned no completion"))?;
            completed(key, &row)
        })));
    }
    let started = Instant::now();
    barrier.wait().await;
    Ok(started)
}

pub(in crate::activity::page_targets::native_tests) fn completion_count(row: &Rec, column: &str) -> Result<u64> {
    crate::read::integer(row, column)?.try_into().map_err(|_| invalid("invalid owned completion"))
}

pub(in crate::activity::page_targets::native_tests) async fn finish<T>(slots: &mut Slots<T>) -> Result<Vec<T>> {
    let mut completed = Vec::new();
    let deadline = tokio::time::Instant::now() + COMPLETION_TIMEOUT;
    for slot in slots.iter_mut() {
        let task = slot.as_mut().ok_or_else(|| invalid("owned worker was already joined"))?;
        let result = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| invalid("owned workers exceeded their shared completion deadline"))?;
        // Cancellation leaves pending handles retained; remove completed handles before propagating errors.
        slot.take();
        completed.push(result.map_err(|_| invalid("owned worker task failed"))??);
    }
    slots.clear();
    Ok(completed)
}

pub(in crate::activity::page_targets::native_tests) fn abort<T>(slots: &Slots<T>) {
    for task in slots.iter().flatten() {
        task.abort();
    }
}

pub(in crate::activity::page_targets::native_tests) async fn stop<T>(slots: &mut Slots<T>) -> Result<()> {
    abort(slots);
    let deadline = tokio::time::Instant::now() + WORKER_CLEANUP_TIMEOUT;
    let mut failure = None;
    for slot in slots.iter_mut() {
        let Some(task) = slot.as_mut() else { continue };
        let result = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| invalid("owned worker cleanup timed out; settlement is unconfirmed"))?;
        slot.take();
        let error = match result {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error),
            Err(error) if error.is_cancelled() => None,
            Err(_) => Some(invalid("owned worker panicked during cleanup")),
        };
        if failure.is_none() {
            failure = error;
        }
    }
    slots.clear();
    failure.map_or(Ok(()), Err)
}
