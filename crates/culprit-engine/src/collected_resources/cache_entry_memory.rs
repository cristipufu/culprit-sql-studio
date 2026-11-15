//! One authenticated native point owns whole cache entries; statements do not receive duplicated memory charges.
use super::*;
use culprit_core::collected_resource_analysis::CollectedCacheEntryMemoryAnalysis;

pub fn cache_entry_memory(capture: &EvidenceCapture) -> Result<CollectedCacheEntryMemoryAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::CacheEntryMemory)?;
    input.database()?;
    let SourceObservation::CacheEntryMemory { focus, .. } = &capture.source().observation else {
        return Err(invalid());
    };
    let context = NativeReadContext::from_snapshot(capture.source())?;
    let observations = input.rows()?;
    for observation in &observations {
        let OperationalFact::MssqlCacheEntryMemory(entry) = &observation.fact else { return Err(invalid()) };
        if !focus.accepts_entry(entry) {
            return Err(invalid());
        }
    }
    let source = input.outcome(observations.len())?;
    let report = culprit_checks::cache_entry_memory::explain(&context, &observations, Some(&source))?;
    Ok(CollectedCacheEntryMemoryAnalysis { context: crate::collected_context::from_capture(capture), report })
}
