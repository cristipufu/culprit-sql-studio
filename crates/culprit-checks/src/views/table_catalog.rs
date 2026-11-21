//! One admission owner for original table and index catalog interpretations; it never reads a connection.
use culprit_core::{Result, SourceOutcome, source_id, telemetry::*};
pub struct MssqlCatalogEvidence<'a> {
    pub description: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub source: &'a SourceOutcome,
    pub read_complete: bool,
}
impl MssqlCatalogEvidence<'_> {
    pub(crate) fn validate(&self, expected: SnapshotDataset) -> Result<NativeReadContext> {
        let expected_source = match expected {
            SnapshotDataset::Tables => source_id::TABLES,
            SnapshotDataset::Indexes => source_id::INDEXES,
            _ => return Err(invalid()),
        };
        let native = self.description.observation.snapshot().ok_or_else(invalid)?;
        let context = NativeReadContext::from_snapshot(self.description)?;
        let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
        if native.dataset != expected
            || context.target.engine() != culprit_core::Engine::Mssql
            || context.target.database().is_none()
            || !matches!(context.partition, SourcePartition::Target)
            || coverage.source != expected_source
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
        let mut database_id = None;
        let mut keys = std::collections::BTreeSet::new();
        for row in self.observations {
            let typed = matches!(
                (expected, &row.fact),
                (SnapshotDataset::Tables, OperationalFact::MssqlTable(_))
                    | (SnapshotDataset::Indexes, OperationalFact::MssqlIndex(_))
            );
            let row_database = match &row.fact {
                OperationalFact::MssqlTable(table) => Some(table.identity.database_id),
                OperationalFact::MssqlIndex(index) => Some(index.table.database_id),
                _ => None,
            };
            let Some(row_database) = row_database else { return Err(invalid()) };
            if database_id.replace(row_database).is_some_and(|before| before != row_database) {
                return Err(invalid());
            }
            if !typed || !row.is_valid() || !keys.insert(&row.key) {
                return Err(invalid());
            }
        }
        Ok(context)
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
