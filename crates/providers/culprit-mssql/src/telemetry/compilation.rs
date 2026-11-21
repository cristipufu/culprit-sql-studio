use super::streamed::{SnapshotAcquisition, SnapshotRecipe, limit_parameter};
use crate::{
    query_store::{costs, reader::ids_parameter},
    read::{NativeRead, Parameter},
    value::Rec,
};
use culprit_core::{Result, Source, telemetry::*};

pub(super) struct QueryCompilation;
pub(super) struct CompilationLayout {
    query_id: Option<String>,
}
#[async_trait::async_trait]
impl SnapshotRecipe for QueryCompilation {
    type Layout = CompilationLayout;
    const DATASET: SnapshotDataset = SnapshotDataset::QueryCompilation;
    const NAME: &'static str = "query-compilation-state";
    const SOURCE: &'static Source = &crate::sources::QUERY_COMPILATION;
    const ACQUISITION: SnapshotAcquisition = SnapshotAcquisition::AzureDatabase;
    const SOURCE_READ: Option<SourceReadQuestion> = Some(SourceReadQuestion::CompilationHistory);
    const REQUIRES_FULL_METADATA: bool = true;

    async fn layout<R: NativeRead>(
        &self,
        _reader: &mut R,
        _budget: &CollectionBudget,
    ) -> Result<Option<CompilationLayout>> {
        Ok(Some(CompilationLayout { query_id: None }))
    }
    fn select(&self, layout: &mut CompilationLayout, request: &CollectionRequest) -> Result<()> {
        layout.query_id = None;
        if let CollectionSelection::SourceRead { focus, .. } = &request.selection
            && let SourceReadPrerequisite::ObservedQueryHistoryFailure { query } = &focus.prerequisite
        {
            // Use only the known native query number for acquisition. Incarnation changes must remain visible
            // to the original comparison; neither a hash nor current text may substitute for that proof.
            ids_parameter(std::slice::from_ref(&query.query_id))?;
            layout.query_id = Some(query.query_id.clone());
        }
        Ok(())
    }
    fn query(&self, layout: &CompilationLayout) -> String {
        if layout.query_id.is_some() { costs::compilation_sql() } else { costs::compilation_snapshot_sql() }
    }
    fn parameters(&self, layout: &CompilationLayout, limit: usize) -> Result<Vec<Parameter>> {
        match &layout.query_id {
            Some(query_id) => Ok(vec![ids_parameter(std::slice::from_ref(query_id))?]),
            None => Ok(limit_parameter(limit)),
        }
    }
    fn decode(
        &self,
        row: &Rec,
        layout: &CompilationLayout,
        _source: &CollectionDescription,
    ) -> Result<TelemetryRecord> {
        let compilation = costs::Compilation::decode(row)?;
        if layout.query_id.as_ref().is_some_and(|query_id| query_id != &compilation.identity.query_id) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(TelemetryRecord::Operational(compilation.record()?))
    }
}
