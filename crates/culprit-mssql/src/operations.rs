//! Native operational reads and their application projection share facts, not rendered labels.
pub(crate) mod native;
use crate::{conn::Pool, probe::Probe, read::ReadSession};
use culprit_core::{
    Coverage, FileKind, FileScope, Snapshot,
    operations::{OperationalCounter, OperationalCounterKind, SpacePart, SpaceUsage},
    telemetry::{CollectionBudget, SourceShutdown},
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
            part.bytes.map(|bytes| SpacePart { name: culprit_core::i18n::t(key, &[]), bytes })
        })
        .collect()
}
fn space(database: &str, fact: native::SpaceFact) -> Option<SpaceUsage> {
    let name = match fact.kind {
        FileKind::Log => log_name(database),
        FileKind::Temp => tempdb_name(),
        _ => filegroup_name(database, fact.native_name.as_deref()?),
    };
    Some(SpaceUsage {
        name,
        kind: fact.kind,
        scope: fact.scope,
        total_bytes: fact.total_bytes?,
        used_bytes: fact.used_bytes?,
        parts: tempdb_parts(&fact.parts),
        growth_enabled: fact.growth_enabled,
        limit_bytes: fact.limit_bytes,
        reuse_wait: fact.reuse_wait,
    })
}
fn attach(snapshot: &mut Snapshot, facts: native::Facts) {
    match facts {
        native::Facts::Spaces(facts) => snapshot
            .operations
            .spaces
            .extend(facts.into_iter().filter_map(|fact| space(&snapshot.server.database, fact))),
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
pub(crate) async fn collect(pool: Arc<Pool>, probe: &Probe, snapshot: &mut Snapshot) {
    let mut coverage = Coverage::default();
    let budget = CollectionBudget { page_bytes: 8 * 1024 * 1024, total_timeout_ms: 120_000, ..Default::default() };
    let mut reader = ReadSession::new(pool, budget);
    for kind in native::Kind::ALL {
        if kind == native::Kind::SuspectPages && probe.is_azure_sql_database() {
            coverage
                .not_applicable(kind.source(), culprit_core::message!("provider.mssql.suspect_pages.not_applicable"));
            continue;
        }
        let result = native::read(&mut reader, kind, 10_000).await.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(probe.grant_hint())
            } else {
                error
            }
        });
        if let Some(facts) = coverage.read(kind.source(), result) {
            coverage.completed(kind.source(), facts.len());
            attach(snapshot, facts);
        }
    }
    if reader.close().await == SourceShutdown::Unconfirmed {
        coverage.note("operations", culprit_core::message!("collection.cancellation_unconfirmed"));
    }
    coverage.append_to(snapshot);
}
