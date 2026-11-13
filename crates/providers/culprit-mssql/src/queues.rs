//! Current queue reads preserve counting units and native request-level database attribution.
use crate::read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_integer};
use culprit_core::{
    Coverage, MeasurementScope, Result, Signal,
    telemetry::{CollectionStop, OperationalFact, OperationalObservation, QueueKind, QueueSample},
};

const MIN_WORKER_WAIT_MS: u32 = 100;
const MIN_COMPILATION_WAIT_MS: u32 = 1000;
macro_rules! queue_sql {
    ($wait:literal, $session:literal, $relations:literal, $predicate:literal) => {
        concat!(
            "SELECT COUNT_BIG(CASE WHEN r.database_id=DB_ID() THEN 1 END) AS confirmed_count,
 COUNT_BIG(CASE WHEN NULLIF(r.database_id,0) IS NULL THEN 1 END) AS unattributed_count,
 MAX(CASE WHEN r.database_id=DB_ID() THEN ",
            $wait,
            " END) AS longest_wait_ms
 FROM ",
            $relations,
            "
 WHERE ",
            $predicate,
            "
 AND ",
            $session,
            " > 0 AND ",
            $session,
            " <> @@SPID AND ISNULL(s.program_name,N'')<>N'culprit'
 AND (r.database_id=DB_ID() OR NULLIF(r.database_id,0) IS NULL)
 OPTION(MAXDOP 1)"
        )
    };
}
const MEMORY: &str = queue_sql!(
    "g.wait_time_ms",
    "g.session_id",
    "sys.dm_exec_query_memory_grants g
 LEFT JOIN sys.dm_exec_sessions s ON s.session_id=g.session_id
 LEFT JOIN sys.dm_exec_requests r ON r.session_id=g.session_id AND r.request_id=g.request_id",
    "g.grant_time IS NULL"
);
const WORKERS: &str = queue_sql!(
    "w.wait_duration_ms",
    "w.session_id",
    "sys.dm_os_waiting_tasks w
 LEFT JOIN sys.dm_exec_sessions s ON s.session_id=w.session_id
 LEFT JOIN sys.dm_os_tasks t ON t.task_address=w.waiting_task_address AND t.session_id=w.session_id
 LEFT JOIN sys.dm_exec_requests r ON r.session_id=t.session_id AND r.request_id=t.request_id",
    "w.wait_type=N'THREADPOOL' AND w.wait_duration_ms>=@P1"
);
const COMPILATION: &str = queue_sql!(
    "r.wait_time",
    "r.session_id",
    "sys.dm_exec_requests r LEFT JOIN sys.dm_exec_sessions s ON s.session_id=r.session_id",
    "r.wait_type=N'RESOURCE_SEMAPHORE_QUERY_COMPILE' AND r.wait_time>=@P1"
);

pub(crate) const KINDS: [QueueKind; 3] =
    [QueueKind::MemoryGrants, QueueKind::WorkerTasks, QueueKind::CompilationRequests];
pub(crate) fn source(kind: QueueKind) -> &'static culprit_core::Source {
    match kind {
        QueueKind::MemoryGrants => &crate::sources::PENDING_GRANTS,
        QueueKind::WorkerTasks => &crate::sources::WORKER_QUEUE,
        QueueKind::CompilationRequests => &crate::sources::COMPILATION_QUEUE,
    }
}
fn minimum_wait_ms(kind: QueueKind) -> u32 {
    match kind {
        QueueKind::MemoryGrants => 0,
        QueueKind::WorkerTasks => MIN_WORKER_WAIT_MS,
        QueueKind::CompilationRequests => MIN_COMPILATION_WAIT_MS,
    }
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, kind: QueueKind) -> Result<QueueSample> {
    let (sql, parameters) = match kind {
        QueueKind::MemoryGrants => (MEMORY, vec![]),
        QueueKind::WorkerTasks => (WORKERS, vec![Parameter::Integer(i64::from(MIN_WORKER_WAIT_MS))]),
        QueueKind::CompilationRequests => (COMPILATION, vec![Parameter::Integer(i64::from(MIN_COMPILATION_WAIT_MS))]),
    };
    let rows = reader.query(sql, parameters, 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(invalid_data());
    };
    let sample = QueueSample {
        kind,
        count: integer(row, "confirmed_count")?.to_string(),
        scope: MeasurementScope::Database,
        unattributed_count: integer(row, "unattributed_count")?.to_string(),
        longest_wait_ms: optional_integer(row, "longest_wait_ms")?.map(|value| value.to_string()),
        minimum_wait_ms: minimum_wait_ms(kind),
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(sample)
}
pub(crate) fn observation(sample: QueueSample) -> OperationalObservation {
    OperationalObservation {
        source: source(sample.kind).name.into(),
        key: match sample.kind {
            QueueKind::MemoryGrants => "memory-grants",
            QueueKind::WorkerTasks => "worker-tasks",
            QueueKind::CompilationRequests => "compilation-requests",
        }
        .into(),
        fact: OperationalFact::Queue(sample),
    }
}
fn record_application_result(coverage: &mut Coverage, kind: QueueKind, result: Result<QueueSample>) -> Option<Signal> {
    let sample = coverage.read(source(kind), result)?;
    let count = sample.count.parse::<u64>().expect("validated native queue count");
    if !sample.attribution_complete() {
        coverage.partial(source(kind), culprit_core::message!("provider.mssql.queues.attribution_unknown"));
        if count == 0 {
            return None;
        }
    }
    let count = count as f64;
    Some(match kind {
        QueueKind::MemoryGrants => crate::pressure::pending_grants_signal(count),
        QueueKind::WorkerTasks => crate::pressure::threadpool_now_signal(count),
        QueueKind::CompilationRequests => crate::pressure::compilation_waiting_signal(count, sample.minimum_wait_ms),
    })
}
pub(crate) async fn application(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
) -> Vec<Signal> {
    let reader = crate::read::ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    collect_queues(reader, probe.grant_hint(), coverage).await
}

async fn collect_queues<R: NativeRead>(mut reader: R, permission_hint: String, coverage: &mut Coverage) -> Vec<Signal> {
    let mut observations = Vec::with_capacity(KINDS.len());
    for kind in KINDS {
        let result = read(&mut reader, kind).await.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(&permission_hint)
            } else {
                error
            }
        });
        observations.push((kind, result));
    }
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        for kind in KINDS {
            coverage.source_failed(source(kind), &CollectionStop::CancellationUnconfirmed.error());
        }
        return Vec::new();
    }
    observations.into_iter().filter_map(|(kind, result)| record_application_result(coverage, kind, result)).collect()
}

#[cfg(test)]
pub(crate) mod tests;
