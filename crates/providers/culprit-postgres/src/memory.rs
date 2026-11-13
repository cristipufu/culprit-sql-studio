//! Scan projection owns the native allocation read until shutdown is confirmed. Allocation size alone is not a
//! pressure assessment; the same native reader/projection serves explicit instance collection.
use crate::{
    connect::Connector,
    native,
    read::{NativeRead, ReadSession},
    sources,
};
use culprit_core::{Coverage, Result, telemetry::*};

const MAX_MEMORY_SNAPSHOT_ROWS: u32 = 4096;
const MAX_MEMORY_SNAPSHOT_QUERIES: u32 = 5;

pub(crate) async fn collect(connector: &Connector, version: u32) -> (Option<Vec<NativeCounterSet>>, Coverage) {
    let mut coverage = Coverage::default();
    if !native::memory::supported(version) {
        coverage.not_applicable(
            &sources::SHARED_MEMORY_ALLOCATIONS,
            culprit_core::message!("provider.postgres.shared_memory_allocations.unavailable_version"),
        );
        return (None, coverage);
    }
    let budget = CollectionBudget {
        max_queries: MAX_MEMORY_SNAPSHOT_QUERIES,
        max_snapshot_rows: MAX_MEMORY_SNAPSHOT_ROWS,
        ..Default::default()
    };
    let mut reader = ReadSession::new(connector.clone(), budget);
    let result = read_snapshot(&mut reader, &connector.host, connector.port).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let counters = coverage.read_rows(&sources::SHARED_MEMORY_ALLOCATIONS, result);
    (counters, coverage)
}

async fn read_snapshot<R: NativeRead>(reader: &mut R, host: &str, port: u16) -> Result<Vec<NativeCounterSet>> {
    let before = native::metadata::read(reader, host, port).await?;
    match native::memory::access(reader, before.version).await? {
        native::memory::Access::Available => {}
        native::memory::Access::Restricted => return Err(native::memory::permission_error()),
        native::memory::Access::Unsupported => return Err(CollectionStop::SourceChanged.error()),
    }
    let values =
        native::memory::read(reader, before.version, &before.started_at, MAX_MEMORY_SNAPSHOT_ROWS as usize).await?;
    let after = native::metadata::read(reader, host, port).await?;
    if !before.same_source(&after) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(values)
}
