use super::*;

/// Native source reads and archive reads use the same classification. The source owner supplies the former;
/// an archive adapter additionally authenticates each immutable receipt before entering `explain`.
pub fn explain_read(read: &CachedRuntimeRead) -> Result<CompletedSpillReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let read_complete = read.read_complete;
    let read_key = read.read_key()?;
    let rows = read
        .observations
        .iter()
        .map(|row| CompletedSpillRecord::native_runtime(read_key.clone(), row))
        .collect::<Result<Vec<_>>>()?;
    let snapshot = read.description.observation.snapshot().ok_or_else(invalid)?;
    let mut counts = BTreeMap::<&str, usize>::new();
    for row in &read.observations {
        *counts.entry(&row.source).or_default() += 1;
    }
    let sources = snapshot
        .sources
        .iter()
        .map(|source| {
            let count = counts.get(source.source.as_str()).copied().unwrap_or(0);
            let missing = !read_complete || count != source.records as usize;
            SourceOutcome {
                source: source.source.clone(),
                state: if missing
                    && matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::Partial)
                {
                    SourceState::Partial
                } else {
                    source.state
                },
                reason: missing.then(|| t!("view.completed_spills.limit.partial")),
                reason_message: None,
            }
        })
        .collect::<Vec<_>>();
    explain(
        CompletedSpillEvidence {
            read_id: &read.read_id,
            source: &read.description,
            rows: &rows,
            sources: &sources,
            read_complete,
        },
        None,
    )
}

/// An independent captured entry may be displayed under unknown scan identity, with an explicit limit.
/// A known database/engine/endpoint/incarnation conflict cannot borrow the scan's namespace.
pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Result<Option<CompletedSpillReport>> {
    let Some(read) = snapshot.operations.cached_runtime.as_deref() else { return Ok(None) };
    match read.description.target.snapshot_namespace_comparison(snapshot) {
        culprit_core::Comparison::Compatible => explain_read(read).map(Some),
        culprit_core::Comparison::Incompatible(culprit_core::ComparisonReason::UnknownIdentity) => explain_read(read)
            .map(|mut report| {
                report.limitations.push(t!("view.observations.limit.namespace"));
                Some(report)
            }),
        _ => Err(invalid()),
    }
}
