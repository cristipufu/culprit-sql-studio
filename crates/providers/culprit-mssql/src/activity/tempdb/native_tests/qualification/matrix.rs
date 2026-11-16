use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const WINDOW: Duration = Duration::from_millis(400);
const MAX_LOAD_BATCHES: usize = 512;
const LOAD_SQL: &str =
    "SET NOCOUNT ON; INSERT dbo.cost_load DEFAULT VALUES; SELECT CONVERT(bigint,@@ROWCOUNT) AS inserted;";

type LoadTask = tokio::task::JoinHandle<culprit_core::Result<(u64, Vec<f64>)>>;
struct LoadWorkers {
    stop: Arc<AtomicBool>,
    tasks: Vec<LoadTask>,
    aborts: Vec<tokio::task::AbortHandle>,
}
impl LoadWorkers {
    fn new(stop: Arc<AtomicBool>) -> Self {
        Self { stop, tasks: vec![], aborts: vec![] }
    }
    fn push(&mut self, task: LoadTask) {
        self.aborts.push(task.abort_handle());
        self.tasks.push(task);
    }
}
impl Drop for LoadWorkers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for task in &self.aborts {
            task.abort();
        }
    }
}

pub(super) async fn abba(fixture: &CostFixture, load: bool) -> culprit_core::Result<Vec<Value>> {
    let mut rounds = Vec::new();
    for source in [false, true, true, false] {
        rounds.push(round(fixture, load, source).await?);
    }
    Ok(rounds)
}
async fn round(fixture: &CostFixture, load: bool, source: bool) -> culprit_core::Result<Value> {
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(tokio::sync::Barrier::new(if load { LOAD_WRITERS + 1 } else { 1 }));
    let mut workers = LoadWorkers::new(stop.clone());
    let setup_started = Instant::now();
    let mut clients = Vec::new();
    if load {
        for _ in 0..LOAD_WRITERS {
            clients.push(fixture.client("cost-fixture-load").await?);
        }
    }
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
    for client in clients {
        workers.push(tokio::spawn(worker(client, stop.clone(), barrier.clone())));
    }
    let started = Instant::now();
    barrier.wait().await;
    let captured = if source { Some(AssertUnwindSafe(capture(fixture)).catch_unwind().await) } else { None };
    let active_at_source_end = workers.tasks.iter().filter(|worker| !worker.is_finished()).count();
    tokio::time::sleep(WINDOW.saturating_sub(started.elapsed())).await;
    stop.store(true, Ordering::Release);
    let mut completed = 0u64;
    let mut latencies = Vec::new();
    let mut errors = Vec::new();
    for mut task in std::mem::take(&mut workers.tasks) {
        match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
            Ok(Ok(Ok((count, mut samples)))) => {
                completed += count;
                latencies.append(&mut samples);
            }
            Ok(Ok(Err(error))) => errors.push(format!("{:?}", error.code)),
            Ok(Err(_)) => errors.push("worker-join-error".into()),
            Err(_) => {
                task.abort();
                let _ = task.await;
                errors.push("worker-settlement-timeout".into());
            }
        }
    }
    latencies.sort_by(f64::total_cmp);
    let elapsed = started.elapsed().as_secs_f64();
    let workload = json!({
        "completed_insert_batches":completed,
        "observed_batch_latencies_ms":latencies,
        "client_elapsed_ms":elapsed*1000.0,
        "completed_batches_per_second":if load {Some(completed as f64/elapsed)}else{None},
        "errors":errors,
        "maximum_batches_per_writer":MAX_LOAD_BATCHES,
        "statistics":"raw per-batch timings; no inferred native CPU or percentiles",
    });
    let captured = match captured {
        Some(Ok(result)) => Some(result?),
        Some(Err(panic)) => std::panic::resume_unwind(panic),
        None => None,
    };
    Ok(json!({
        "source_enabled":source,
        "load_enabled":load,
        "load_connection_setup_client_elapsed_ms":setup_ms,
        "workload":workload,
        "active_load_writers_at_source_end":if load && source {Some(active_at_source_end)}else{None},
        "sustained_load_established":if load && source {Some(active_at_source_end>0)}else{None},
        "source_work":captured,
        "observer_effect_included_in_workload_comparison":true,
    }))
}
async fn worker(
    mut client: crate::conn::MsClient,
    stop: Arc<AtomicBool>,
    barrier: Arc<tokio::sync::Barrier>,
) -> culprit_core::Result<(u64, Vec<f64>)> {
    let mut latencies = Vec::new();
    let mut completed = 0;
    barrier.wait().await;
    while !stop.load(Ordering::Acquire) && latencies.len() < MAX_LOAD_BATCHES {
        let started = Instant::now();
        let row = client
            .simple_query(LOAD_SQL)
            .await
            .map_err(map_error)?
            .into_row()
            .await
            .map_err(map_error)?
            .ok_or_else(crate::read::invalid_data)?;
        let inserted = crate::read::integer(&crate::value::Rec::from_row(&row), "inserted")?;
        assert_eq!(inserted, 1);
        completed += 1;
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok((completed, latencies))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_load_settlement_aborts_a_worker_even_after_its_join_handle_was_taken() {
        struct Released(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Released {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let (released, wait_for_release) = tokio::sync::oneshot::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = LoadWorkers::new(stop.clone());
        workers.push(tokio::spawn(async move {
            let _released = Released(Some(released));
            std::future::pending::<()>().await;
            Ok((0, vec![]))
        }));
        tokio::task::yield_now().await;
        // An outer timeout can drop the local join future while settlement awaits it. The group retains
        // cancellation ownership separately; dropping this handle cannot leave the worker detached.
        let awaiting = workers.tasks.pop().unwrap();
        drop(awaiting);
        drop(workers);
        assert!(stop.load(Ordering::Acquire));
        tokio::time::timeout(Duration::from_secs(1), wait_for_release).await.unwrap().unwrap();
    }
}
