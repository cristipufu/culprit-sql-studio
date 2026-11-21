//! One admission owner for original table-catalog interpretations; it never reads a connection.
use culprit_core::{Result, SourceOutcome, source_id, telemetry::*};
pub struct TableCatalogEvidence<'a> {
    pub description: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub source: &'a SourceOutcome,
    pub read_complete: bool,
}
impl TableCatalogEvidence<'_> {
    pub(crate) fn validate(&self) -> Result<NativeReadContext> {
        let native = self.description.observation.snapshot().ok_or_else(invalid)?;
        let context = NativeReadContext::from_snapshot(self.description)?;
        let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
        if native.dataset != SnapshotDataset::Tables
            || context.target.engine() != culprit_core::Engine::Mssql
            || context.target.database().is_none()
            || !matches!(context.partition, SourcePartition::Target)
            || coverage.source != source_id::TABLES
            || self.source.source != coverage.source
            || self.observations.len() > coverage.records as usize
            || self.read_complete && self.observations.len() != coverage.records as usize
            || !super::source_coverage::consistent_source(
                Some(self.source),
                self.observations.iter().map(|row| row.source.as_str()),
            )
        {
            return Err(invalid());
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in self.observations {
            if !matches!(row.fact, OperationalFact::MssqlTable(_)) || !row.is_valid() || !keys.insert(&row.key) {
                return Err(invalid());
            }
        }
        Ok(context)
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
