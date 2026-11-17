use super::*;

pub(super) fn capture(input: &CompletedSpillEvidence<'_>, dataset: SnapshotDataset) -> Result<NativeReadContext> {
    let SourceObservation::Snapshot(snapshot) = &input.source.observation else { return Err(invalid()) };
    let context = NativeReadContext::from_snapshot(input.source).map_err(|_| invalid())?;
    if !valid_digest(input.read_id)
        || snapshot.dataset != dataset
        || input.source.target.engine() != Engine::Mssql
        || input.source.target.database().is_none()
        || input.rows.len() > snapshot.records as usize
        || input.sources.len() != snapshot.sources.len()
    {
        return Err(invalid());
    }
    if dataset == SnapshotDataset::CachedRuntime
        && !CachedRuntimeRead::accepts_rows(
            input.source,
            input.rows.iter().filter_map(CompletedSpillRecord::operational),
            input.read_complete,
        )
    {
        return Err(invalid());
    }
    let read_key = cached_statement_read_key(input.read_id, input.source).map_err(|_| invalid())?;
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut keys = BTreeSet::new();
    let mut origin = None;
    let mut capture = None;
    for row in input.rows {
        if !row.reference.is_valid()
            || !row.is_valid()
            || row.stream() != dataset.stream()
            || !keys.insert(row.native_key()?)
        {
            return Err(invalid());
        }
        let captured = matches!(row.reference, CompletedSpillReference::Captured { .. });
        if origin.is_some_and(|original| original != captured) {
            return Err(invalid());
        }
        origin = Some(captured);
        match &row.reference {
            CompletedSpillReference::Captured { reference } => {
                if input.read_id != cached_statement_read_id(&reference.capture).map_err(|_| invalid())?
                    || reference.row_ordinal >= snapshot.records
                    || capture.as_ref().is_some_and(|original| original != &reference.capture_ref())
                {
                    return Err(invalid());
                }
                capture = Some(reference.capture_ref());
                reference.validate_canonical_record(input.source, &row.to_record()).map_err(|_| invalid())?;
            }
            CompletedSpillReference::NativeRead { read_key: supplied, observation_key } => {
                if row.runtime().is_none() || supplied != &read_key || *observation_key != row.native_key()? {
                    return Err(invalid());
                }
            }
        }
        match dataset {
            SnapshotDataset::CachedRuntime if row.runtime().is_none() => return Err(invalid()),
            SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans => {
                let definition = row.definition().ok_or_else(invalid)?;
                if !matches!(
                    (&definition.fact, dataset),
                    (ObjectDefinitionFact::MssqlCachedDefinition(_), SnapshotDataset::CachedDefinitions)
                        | (ObjectDefinitionFact::MssqlRetainedPlan(_), SnapshotDataset::RetainedPlans)
                ) || !definition.fact.native_plan_proofs_match(input.source)
                {
                    return Err(invalid());
                }
            }
            _ => {}
        }
        if !snapshot.sources.iter().any(|coverage| coverage.source == row.source()) {
            return Err(invalid());
        }
        *counts.entry(row.source()).or_default() += 1;
    }
    let mut declared = BTreeSet::new();
    for outcome in input.sources {
        if !declared.insert(&outcome.source) {
            return Err(invalid());
        }
        let native = snapshot.sources.iter().find(|source| source.source == outcome.source).ok_or_else(invalid)?;
        let rows = counts.get(outcome.source.as_str()).copied().unwrap_or(0);
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
