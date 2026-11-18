//! Native operational reads and their application projection share facts, not rendered labels.
pub(crate) mod native;
use crate::{conn::Pool, probe::Probe, read::ReadSession};
use culprit_core::{
    Coverage, FileKind, FileScope, Snapshot,
    operations::{OperationalCounter, OperationalCounterKind, SpacePart, SpaceUsage},
};
use std::sync::Arc;
/// This database's transaction log, as a space.
pub(crate) fn log_name(database: &str) -> String {
    culprit_core::t!("provider.mssql.space.log", database = database)
}

/// One of this database's filegroups, as a space.
pub(crate) fn filegroup_name(database: &str, filegroup: &str) -> String {
    culprit_core::t!("provider.mssql.space.filegroup", database = database, filegroup = filegroup)
}

/// tempdb's data files, shared by every database on the instance.
pub(crate) fn tempdb_name() -> String {
    culprit_core::t!("provider.mssql.space.tempdb")
}

pub(crate) fn tempdb_parts(parts: &[native::SpacePart]) -> Vec<SpacePart> {
    parts
        .iter()
        .filter_map(|part| {
            let key = match part.kind {
                native::SpacePartKind::Users => culprit_core::key!("provider.mssql.space.user_objects"),
                native::SpacePartKind::Internals => culprit_core::key!("provider.mssql.space.internal_objects"),
                native::SpacePartKind::Versions => culprit_core::key!("provider.mssql.version_store"),
            };
            part.bytes
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .map(|bytes| SpacePart { name: culprit_core::i18n::t(key, &[]), bytes: bytes as f64 })
        })
        .collect()
}
fn space(database: &str, fact: native::SpaceFact, source: &str) -> Option<SpaceUsage> {
    let name = match fact.kind {
        FileKind::Log => log_name(database),
        FileKind::Temp => tempdb_name(),
        _ => filegroup_name(database, fact.native_name.as_deref()?),
    };
    Some(SpaceUsage {
        origin: Some(culprit_core::telemetry::StoragePointRef { source: source.into(), key: fact.key.clone() }),
        name,
        kind: fact.kind,
        scope: fact.scope,
        total_bytes: fact.total_bytes.as_deref()?.parse::<u64>().ok()? as f64,
        used_bytes: fact.used_bytes.as_deref()?.parse::<u64>().ok()? as f64,
        parts: tempdb_parts(&fact.parts),
        growth_enabled: fact.growth_enabled,
        limit_bytes: fact.limit_bytes.as_deref().and_then(|value| value.parse::<u64>().ok()).map(|value| value as f64),
        reuse_wait: fact.reuse_wait,
    })
}
fn attach(snapshot: &mut Snapshot, facts: native::Facts, source: &str) {
    match facts {
        native::Facts::Spaces(facts) | native::Facts::Files { spaces: facts, .. } => snapshot
            .operations
            .spaces
            .extend(facts.into_iter().filter_map(|fact| space(&snapshot.server.database, fact, source))),
        native::Facts::Sequences(sequences) => {
            snapshot.operations.sequences.extend(sequences.into_iter().map(|fact| fact.capacity))
        }
        native::Facts::SuspectPages(fact) if fact.count > 0 => snapshot.operations.counters.push(OperationalCounter {
            kind: OperationalCounterKind::ChecksumFailures,
            subject: snapshot.server.database.clone(),
            scope: FileScope::Database,
            count: fact.count as f64,
            since: None,
            last_event_at: None,
            before: None,
        }),
        native::Facts::SuspectPages(_) => {}
    }
}
pub(crate) async fn collect(
    pool: Arc<Pool>,
    probe: &Probe,
    target: culprit_core::telemetry::CollectionTarget,
    snapshot: &mut Snapshot,
) {
    let budget = culprit_core::telemetry::CollectionBudget {
        page_bytes: 8 * 1024 * 1024,
        total_timeout_ms: 120_000,
        ..Default::default()
    };
    let mut reader = ReadSession::new(pool, budget);
    collect_with(&mut reader, probe, target, snapshot).await;
}
async fn collect_with<R: crate::read::NativeRead>(
    reader: &mut R,
    probe: &Probe,
    target: culprit_core::telemetry::CollectionTarget,
    snapshot: &mut Snapshot,
) {
    use culprit_core::telemetry::*;
    let started = std::time::SystemTime::now();
    let mut coverage = Coverage::default();
    let mut staged = Vec::new();
    let mut stopped: Option<culprit_core::Error> = None;
    for kind in native::Kind::ALL {
        if kind == native::Kind::SuspectPages && probe.is_azure_sql_database() {
            coverage
                .not_applicable(kind.source(), culprit_core::message!("provider.mssql.suspect_pages.not_applicable"));
            continue;
        }
        let result = match &stopped {
            Some(error) => Err(error.clone()),
            None => read_kind(reader, kind, &snapshot.server.database).await,
        };
        if let Err(error) = &result
            && crate::read::stops_remaining_reads(error)
        {
            stopped = Some(error.clone());
        }
        staged.push((kind, result));
    }
    let shutdown = reader.close().await;
    let mut points = Vec::new();
    let mut sources = Vec::new();
    for (kind, result) in staged {
        let result = if shutdown == SourceShutdown::Unconfirmed {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else {
            result
        }
        .map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(probe.grant_hint())
            } else {
                error
            }
        });
        if StorageRead::recognizes(kind.source().name) {
            sources.push(match &result {
                Ok((facts, _)) => StateSourceCoverage::collected(kind.source().name.into(), facts.len() as u32),
                Err(error) => StateSourceCoverage::unavailable(
                    kind.source().name.into(),
                    error.code,
                    Some(CollectionStop::from_error(error)),
                ),
            });
        }
        if let Some((facts, observations)) = coverage.read(kind.source(), result) {
            coverage.completed(kind.source(), facts.len());
            points.extend(observations.into_iter().filter(|row| StorageRead::recognizes(&row.source)));
            attach(snapshot, facts, kind.source().name);
        }
    }
    let result = ObservationReadContext::collector(started, std::time::SystemTime::now()).and_then(|context| {
        let read = StorageRead {
            target,
            context,
            sources,
            observations: points,
            read_complete: shutdown == SourceShutdown::Quiescent,
        };
        read.is_valid().then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
    });
    match result {
        Ok(read) => snapshot.operations.storage = Some(Box::new(read)),
        Err(error) => {
            coverage.source_failed(&crate::sources::FILE_CAPACITY, &error);
        }
    }
    coverage.append_to(snapshot);
}
async fn read_kind<R: crate::read::NativeRead>(
    reader: &mut R,
    kind: native::Kind,
    database: &str,
) -> culprit_core::Result<(native::Facts, Vec<culprit_core::telemetry::OperationalObservation>)> {
    use culprit_core::telemetry::*;
    let facts = native::read(reader, kind, 10_000).await?;
    let observations = facts.clone().observations(kind)?;
    if observations.iter().any(|row| match &row.fact {
        OperationalFact::Space(point) => point.database.as_deref().is_some_and(|name| name != database),
        OperationalFact::MssqlDatabaseFile(point) => point.database != database,
        _ => false,
    }) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok((facts, observations))
}

#[cfg(test)]
mod tests;
