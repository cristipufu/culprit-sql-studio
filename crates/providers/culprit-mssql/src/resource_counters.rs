//! Opt-in shared metadata for a scan. Native readers also serve collection; no pressure verdict or query owner
//! follows from these values alone. Their server work must be load-qualified before any default profile enables them.
use crate::{
    conn::Pool,
    memory_clerks,
    probe::Probe,
    read::{NativeRead, ReadSession, stops_remaining_reads},
    schedulers, sources,
};
use culprit_core::{Coverage, ErrorCode, telemetry::*};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Default)]
pub(crate) struct SharedResourceRead {
    pub counters: Option<Vec<NativeCounterSet>>,
    pub instance_resources: Option<Box<InstanceResourceRead>>,
    pub coverage: Coverage,
}

const MAX_ROWS_PER_SOURCE: usize = 500;
const MAX_QUERIES: u32 = 4; // Clerk groups, scheduler columns/rows, and supported native instance point.
const STATEMENT_TIMEOUT_MS: u32 = 3000;
const TOTAL_TIMEOUT_MS: u32 = 10_000;

fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: MAX_ROWS_PER_SOURCE as u32,
        page_rows: MAX_ROWS_PER_SOURCE as u32,
        statement_timeout_ms: STATEMENT_TIMEOUT_MS,
        total_timeout_ms: TOTAL_TIMEOUT_MS,
        ..Default::default()
    }
}

pub(crate) async fn collect(pool: Arc<Pool>, probe: &Probe, requested: bool) -> SharedResourceRead {
    let mut reader = ReadSession::new(pool, budget());
    // Azure shared-view grants vary by service objective/pool. The database-only grant hint would be misleading.
    let grant = (!probe.is_azure_sql_database()).then(|| probe.grant_hint());
    collect_from(
        &mut reader,
        requested,
        probe.major >= u32::from(memory_clerks::MIN_SQL_SERVER_MAJOR),
        crate::instance::supports_profile(probe.engine_edition, probe.major),
        grant.as_deref(),
    )
    .await
}

async fn collect_from<R: NativeRead>(
    reader: &mut R,
    requested: bool,
    memory_supported: bool,
    instance_supported: bool,
    grant: Option<&str>,
) -> SharedResourceRead {
    let mut coverage = Coverage::default();
    if !requested {
        for source in [&sources::MEMORY_CLERKS, &sources::SCHEDULERS, &sources::INSTANCE_RESOURCES] {
            coverage.not_applicable(source, culprit_core::message!("provider.shared_resources.not_requested"));
        }
        return SharedResourceRead { coverage, ..Default::default() };
    }
    let clerks = if memory_supported {
        Some(memory_clerks::read(reader, MAX_ROWS_PER_SOURCE).await)
    } else {
        coverage.not_applicable(
            &sources::MEMORY_CLERKS,
            culprit_core::message!("provider.mssql.shared_resources.memory_version"),
        );
        None
    };
    let schedulers =
        match clerks.as_ref().and_then(|result| result.as_ref().err()).filter(|error| stops_remaining_reads(error)) {
            Some(error) => Err(error.clone()),
            None => schedulers::read(reader, MAX_ROWS_PER_SOURCE).await,
        };
    let from = SystemTime::now();
    let instance = if instance_supported {
        let prior = schedulers.as_ref().err().filter(|error| stops_remaining_reads(error));
        Some(match prior {
            Some(error) => Err(error.clone()),
            None => crate::resources::read_instance_pressure(reader)
                .await
                .map_err(crate::read::ReadError::into_error)
                .and_then(|point: crate::resources::InstancePressurePoint| point.observation()),
        })
    } else {
        coverage.not_applicable(
            &sources::INSTANCE_RESOURCES,
            culprit_core::message!("provider.mssql.instance_resources.unsupported"),
        );
        None
    };
    let context = ObservationReadContext::collector(from, SystemTime::now());
    let shutdown = reader.close().await;
    let mut counters = Vec::new();
    let mut observed = false;
    for (source, result) in [(&sources::MEMORY_CLERKS, clerks), (&sources::SCHEDULERS, Some(schedulers))] {
        let Some(result) = result else { continue };
        let result = if shutdown == SourceShutdown::Unconfirmed {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else {
            result
        };
        let result = result.map_err(|error| with_permission_hint(error, grant));
        if let Some(mut samples) = coverage.read_rows(source, result) {
            coverage.completed(source, samples.len());
            observed = true;
            counters.append(&mut samples);
        }
    }
    let instance_resources = instance.and_then(|result| {
        let result = if shutdown == SourceShutdown::Unconfirmed {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else {
            result
        };
        let result = result
            .and_then(|point| context.map(|context| (point, context)))
            .map_err(|error| with_permission_hint(error, grant));
        let (point, context) = coverage.read(&sources::INSTANCE_RESOURCES, result)?;
        coverage.completed(&sources::INSTANCE_RESOURCES, 1);
        let complete = point.has_complete_population();
        if !complete {
            coverage.partial(
                &sources::INSTANCE_RESOURCES,
                culprit_core::message!("view.instance_resources.limit.population"),
            );
        }
        Some(Box::new(InstanceResourceRead {
            context,
            source: StateSourceCoverage::observed(INSTANCE_RESOURCES_SOURCE.into(), 1, complete),
            observation: Some(Box::new(point)),
            read_complete: true,
        }))
    });
    SharedResourceRead { counters: observed.then_some(counters), instance_resources, coverage }
}

fn with_permission_hint(error: culprit_core::Error, grant: Option<&str>) -> culprit_core::Error {
    if error.code == ErrorCode::PermissionDenied
        && let Some(grant) = grant
    {
        error.with_hint(grant.to_owned())
    } else {
        error
    }
}

#[cfg(test)]
mod tests;
