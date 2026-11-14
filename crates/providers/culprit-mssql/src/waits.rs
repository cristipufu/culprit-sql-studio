//! Native cumulative waits. Export keeps nonzero native types; app projections choose relevant categories.
use crate::read::{NativeRead, ReadResult, integer, invalid_data, text};
use culprit_core::{MeasurementScope, ResetMarker, WaitStat, source_id, telemetry::WaitCounterObservation};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) type WaitTotals = HashMap<String, (f64, f64, f64)>;
pub(crate) const MAX_WAIT_TYPES: usize = 4096;

pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    database_scope: bool,
    limit: usize,
) -> ReadResult<Vec<WaitCounterObservation>> {
    let view = if database_scope { "sys.dm_db_wait_stats" } else { "sys.dm_os_wait_stats" };
    let sql = format!(
        "SELECT wait_type, waiting_tasks_count, wait_time_ms, signal_wait_time_ms, max_wait_time_ms FROM {view}
WHERE waiting_tasks_count > 0 OR wait_time_ms > 0 OR signal_wait_time_ms > 0 OR max_wait_time_ms > 0
OPTION (MAXDOP 1)"
    );
    let rows = reader.query(&sql, vec![], limit).await?;
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let observation = WaitCounterObservation {
                source: source_id::WAITS.into(),
                wait_type: text(row, "wait_type")?,
                scope: if database_scope { MeasurementScope::Database } else { MeasurementScope::Server },
                // SQLPERF can reset waits independently of server/database startup; the view exposes no reset generation.
                reset: ResetMarker::Unknown,
                waiting_tasks_count: integer(row, "waiting_tasks_count")?.to_string(),
                wait_time_ms: integer(row, "wait_time_ms")?.to_string(),
                signal_wait_time_ms: integer(row, "signal_wait_time_ms")?.to_string(),
                max_wait_time_ms: integer(row, "max_wait_time_ms")?.to_string(),
            };
            if !observation.is_valid() || !seen.insert(observation.wait_type.clone()) {
                return Err(invalid_data().into());
            }
            Ok(observation)
        })
        .collect()
}

pub(crate) fn totals(rows: &[WaitCounterObservation]) -> WaitTotals {
    rows.iter()
        .filter(|row| relevant(&row.wait_type) && number(&row.wait_time_ms) > 0.0)
        .map(|row| {
            (
                row.wait_type.clone(),
                (number(&row.wait_time_ms), number(&row.signal_wait_time_ms), number(&row.waiting_tasks_count)),
            )
        })
        .collect()
}
/// A regression anywhere in a complete native snapshot invalidates the whole short sample.
/// A reset that catches up between reads remains unknowable: exported observations keep Unknown reset evidence.
pub(crate) fn deltas(
    before: &[WaitCounterObservation],
    after: &[WaitCounterObservation],
) -> culprit_core::Result<WaitTotals> {
    let by_type: HashMap<_, _> = after.iter().map(|row| (&row.wait_type, row)).collect();
    for previous in before {
        let Some(current) = by_type.get(&previous.wait_type) else {
            return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
        };
        if previous.scope != current.scope
            || previous.reset != current.reset
            || values(previous).into_iter().zip(values(current)).any(|(old, new)| new < old)
        {
            return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
        }
    }
    let previous: HashMap<_, _> = before.iter().map(|row| (&row.wait_type, values(row))).collect();
    Ok(after
        .iter()
        .filter(|row| relevant(&row.wait_type))
        .filter_map(|row| {
            let old = previous.get(&row.wait_type).copied().unwrap_or_default();
            let new = values(row);
            let [tasks, ms, signal, _] = std::array::from_fn::<_, 4, _>(|index| new[index] - old[index]);
            (ms > 0).then_some((row.wait_type.clone(), (ms as f64, signal as f64, tasks as f64)))
        })
        .collect())
}
fn values(row: &WaitCounterObservation) -> [u64; 4] {
    [&row.waiting_tasks_count, &row.wait_time_ms, &row.signal_wait_time_ms, &row.max_wait_time_ms]
        .map(|value| value.parse().expect("validated native count"))
}

fn number(value: &str) -> f64 {
    value.parse::<u64>().expect("native counters were validated") as f64
}
pub(crate) fn relevant(wait: &str) -> bool {
    !BENIGN_WAITS.contains(&wait) && !wait.starts_with("SLEEP_")
}

pub(crate) fn categories(totals: &WaitTotals) -> Vec<WaitStat> {
    let mut categories: BTreeMap<&str, (f64, f64, String, f64)> = BTreeMap::new();
    for (wait_type, (ms, _, tasks)) in totals {
        let entry = categories.entry(wait_type_category(wait_type)).or_default();
        entry.0 += ms;
        entry.1 += tasks;
        if *ms > entry.3 || (*ms == entry.3 && (entry.2.is_empty() || wait_type < &entry.2)) {
            entry.2 = wait_type.clone();
            entry.3 = *ms;
        }
    }
    let mut result: Vec<_> = categories
        .into_iter()
        .map(|(category, (ms, tasks, top, _))| WaitStat {
            category: category.into(),
            wait_type: Some(top),
            wait_ms: ms,
            tasks: Some(tasks),
        })
        .collect();
    result.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms).then_with(|| a.category.cmp(&b.category)));
    result
}

/// Waits that accumulate while SQL Server is idle or doing background work; they say nothing about query performance.
pub(crate) const BENIGN_WAITS: &[&str] = &[
    "BROKER_EVENTHANDLER",
    "BROKER_RECEIVE_WAITFOR",
    "BROKER_TASK_STOP",
    "BROKER_TO_FLUSH",
    "BROKER_TRANSMITTER",
    "CHECKPOINT_QUEUE",
    "CHKPT",
    "CLR_AUTO_EVENT",
    "CLR_MANUAL_EVENT",
    "CLR_SEMAPHORE",
    "DBMIRROR_DBM_EVENT",
    "DBMIRROR_EVENTS_QUEUE",
    "DBMIRROR_WORKER_QUEUE",
    "DBMIRRORING_CMD",
    "DIRTY_PAGE_POLL",
    "DISPATCHER_QUEUE_SEMAPHORE",
    "EXECSYNC",
    "FSAGENT",
    "FT_IFTS_SCHEDULER_IDLE_WAIT",
    "FT_IFTSHC_MUTEX",
    "HADR_CLUSAPI_CALL",
    "HADR_FILESTREAM_IOMGR_IOCOMPLETION",
    "HADR_LOGCAPTURE_WAIT",
    "HADR_NOTIFICATION_DEQUEUE",
    "HADR_TIMER_TASK",
    "HADR_WORK_QUEUE",
    "KSOURCE_WAKEUP",
    "LAZYWRITER_SLEEP",
    "LOGMGR_QUEUE",
    "MEMORY_ALLOCATION_EXT",
    "ONDEMAND_TASK_QUEUE",
    "PARALLEL_REDO_DRAIN_WORKER",
    "PARALLEL_REDO_LOG_CACHE",
    "PARALLEL_REDO_TRAN_LIST",
    "PARALLEL_REDO_WORKER_SYNC",
    "PARALLEL_REDO_WORKER_WAIT_WORK",
    "PREEMPTIVE_OS_FLUSHFILEBUFFERS",
    "PREEMPTIVE_XE_GETTARGETSTATE",
    "PVS_PREALLOCATE",
    "PWAIT_ALL_COMPONENTS_INITIALIZED",
    "PWAIT_DIRECTLOGCONSUMER_GETNEXT",
    "PWAIT_EXTENSIBILITY_CLEANUP_TASK",
    "QDS_PERSIST_TASK_MAIN_LOOP_SLEEP",
    "QDS_ASYNC_QUEUE",
    "QDS_CLEANUP_STALE_QUERIES_TASK_MAIN_LOOP_SLEEP",
    "QDS_SHUTDOWN_QUEUE",
    "REDO_THREAD_PENDING_WORK",
    "REQUEST_FOR_DEADLOCK_SEARCH",
    "RESOURCE_QUEUE",
    "SERVER_IDLE_CHECK",
    "SLEEP_BPOOL_FLUSH",
    "SLEEP_DBSTARTUP",
    "SLEEP_DCOMSTARTUP",
    "SLEEP_MASTERDBREADY",
    "SLEEP_MASTERMDREADY",
    "SLEEP_MASTERUPGRADED",
    "SLEEP_MSDBSTARTUP",
    "SLEEP_SYSTEMTASK",
    "SLEEP_TASK",
    "SLEEP_TEMPDBSTARTUP",
    "SNI_HTTP_ACCEPT",
    "SOS_WORK_DISPATCHER",
    "SP_SERVER_DIAGNOSTICS_SLEEP",
    "SQLTRACE_BUFFER_FLUSH",
    "SQLTRACE_INCREMENTAL_FLUSH_SLEEP",
    "SQLTRACE_WAIT_ENTRIES",
    "UCS_SESSION_REGISTRATION",
    "VDI_CLIENT_OTHER",
    "WAIT_FOR_RESULTS",
    "WAITFOR",
    "WAITFOR_TASKSHUTDOWN",
    "WAIT_XTP_RECOVERY",
    "WAIT_XTP_HOST_WAIT",
    "WAIT_XTP_OFFLINE_CKPT_NEW_LOG",
    "WAIT_XTP_CKPT_CLOSE",
    "XE_DISPATCHER_JOIN",
    "XE_DISPATCHER_WAIT",
    "XE_TIMER_EVENT",
    "XE_LIVE_TARGET_TVF",
    "SOS_SCHEDULER_YIELD_IDLE",
    "BROKER_FORWARDER",
    "HADR_DATABASE_FLOW_CONTROL_IDLE",
];

pub(crate) fn is_lock_wait_type(wait_type: &str) -> bool {
    wait_type.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("LCK_"))
}
pub(crate) fn wait_type_category(wait_type: &str) -> &'static str {
    let w = wait_type.to_ascii_uppercase();
    if is_lock_wait_type(&w) {
        return "Lock";
    }
    if w.starts_with("PAGEIOLATCH") || w == "IO_COMPLETION" || w == "ASYNC_IO_COMPLETION" || w.starts_with("BACKUPIO") {
        return "IO";
    }
    if w.starts_with("PAGELATCH") || w.starts_with("LATCH_") {
        return "Latch";
    }
    if w == "WRITELOG"
        || w.starts_with("LOG_RATE_GOVERNOR")
        || w.contains("LOG_RATE_GOVERNOR")
        || w.starts_with("LOGBUFFER")
        || w.starts_with("HADR_SYNC_COMMIT")
    {
        return "Log";
    }
    if w == "THREADPOOL" {
        return "Workers";
    }
    if w == "SOS_SCHEDULER_YIELD" || w.starts_with("RESOURCE_GOVERNOR") {
        return "CPU";
    }
    // HT* and BMP* are batch-mode hash and bitmap work shared across a parallel query's threads.
    if w.starts_with("CXPACKET")
        || w.starts_with("CXCONSUMER")
        || w.starts_with("CXSYNC")
        || w.starts_with("EXCHANGE")
        || w.starts_with("HT")
        || w.starts_with("BMP")
    {
        return "Parallelism";
    }
    if w == "RESOURCE_SEMAPHORE_QUERY_COMPILE" {
        return "Compilation";
    }
    if w.starts_with("RESOURCE_SEMAPHORE") || w == "CMEMTHREAD" {
        return "Memory";
    }
    if w == "ASYNC_NETWORK_IO" || w.starts_with("NET_") {
        return "Network";
    }
    "Other"
}

#[cfg(test)]
pub(crate) mod tests;
