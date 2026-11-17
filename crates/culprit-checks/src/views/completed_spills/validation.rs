use super::*;

pub(super) fn capture(input: &CompletedSpillEvidence<'_>, dataset: SnapshotDataset) -> Result<NativeReadContext> {
    let SourceObservation::Snapshot(snapshot) = &input.source.observation else { return Err(invalid()) };
    let context = NativeReadContext::from_snapshot(input.source).map_err(|_| invalid())?;
    if snapshot.dataset != dataset
        || input.source.target.engine() != Engine::Mssql
        || input.source.target.database().is_none()
        || input.rows.len() > snapshot.records as usize
        || input.sources.len() != snapshot.sources.len()
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    for row in input.rows {
        if !row.reference.is_valid()
            || row.reference.row_ordinal >= snapshot.records
            || !row.record.is_valid()
            || row.record.stream() != dataset.stream()
        {
            return Err(invalid());
        }
        row.reference.validate_canonical_record(input.source, row.record).map_err(|_| invalid())?;
        if !keys.insert(row.reference.record_id.clone()) {
            return Err(invalid());
        }
        let source = match row.record {
            TelemetryRecord::Operational(OperationalObservation {
                source,
                fact: OperationalFact::MssqlCachedRuntime(_),
                ..
            }) if dataset == SnapshotDataset::CachedRuntime => source,
            TelemetryRecord::ObjectDefinition(definition)
                if matches!(dataset, SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans) =>
            {
                if !matches!(
                    (&definition.fact, dataset),
                    (ObjectDefinitionFact::MssqlCachedDefinition(_), SnapshotDataset::CachedDefinitions)
                        | (ObjectDefinitionFact::MssqlRetainedPlan(_), SnapshotDataset::RetainedPlans)
                ) || !definition.fact.native_plan_proofs_match(input.source)
                {
                    return Err(invalid());
                }
                &definition.source
            }
            _ => return Err(invalid()),
        };
        if !snapshot.sources.iter().any(|coverage| coverage.source == *source) {
            return Err(invalid());
        }
    }
    let mut declared = BTreeSet::new();
    for outcome in input.sources {
        if !declared.insert(&outcome.source) {
            return Err(invalid());
        }
        let native = snapshot.sources.iter().find(|source| source.source == outcome.source).ok_or_else(invalid)?;
        let rows = input
            .rows
            .iter()
            .filter(|row| match row.record {
                TelemetryRecord::Operational(row) => row.source == outcome.source,
                TelemetryRecord::ObjectDefinition(row) => row.source == outcome.source,
                _ => false,
            })
            .count();
        if rows > native.records as usize
            || outcome.state != native.state
                && !(outcome.state == SourceState::Partial
                    && matches!(native.state, SourceState::Collected | SourceState::Empty | SourceState::Partial))
            || match outcome.state {
                SourceState::Collected => rows == 0 || rows != native.records as usize || !input.read_complete,
                SourceState::Empty => rows > 0 || native.records > 0,
                SourceState::Unavailable | SourceState::NotApplicable => rows > 0 || outcome.state != native.state,
                SourceState::Partial => false,
            }
        {
            return Err(invalid());
        }
    }
    Ok(context)
}
