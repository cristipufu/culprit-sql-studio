//! Evidence reads need query credentials, never an ingestion transport or source database connection.
use crate::{client::QueryTransport, sink::ClickHouseStore};
use culprit_collector::analytical::{evidence::EvidenceReader, indexes::ProjectionIndexes};
use culprit_core::Result;
use std::sync::Arc;

pub fn new(query: Arc<dyn QueryTransport>, indexes: Arc<ProjectionIndexes>) -> Result<EvidenceReader> {
    Ok(EvidenceReader::new(Arc::new(ClickHouseStore::new(query)?), indexes))
}

#[cfg(test)]
mod tests;
