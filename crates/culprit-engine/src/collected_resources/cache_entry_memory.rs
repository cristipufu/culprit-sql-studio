//! Authenticated original selections own independent native cache points and their shared comparison.
use super::*;
use culprit_checks::cache_entry_memory::CacheEntryMemoryEvidence;
use culprit_core::collected_resource_analysis::CollectedCacheEntryMemoryAnalysis;

pub fn cache_entry_memory(
    capture: &EvidenceCapture,
    baseline: Option<&EvidenceCapture>,
) -> Result<CollectedCacheEntryMemoryAnalysis> {
    if baseline.is_some_and(|baseline| baseline.range_id() == capture.range_id()) {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    let current = CapturedEntries::new(capture)?;
    let report = if let Some(baseline) = baseline {
        let previous = CapturedEntries::new(baseline)?;
        culprit_checks::cache_entry_memory::compare(previous.evidence(), current.evidence())?
    } else {
        culprit_checks::cache_entry_memory::explain(&current.context, &current.observations, Some(&current.source))?
    };
    Ok(CollectedCacheEntryMemoryAnalysis {
        context: crate::collected_context::from_capture(capture),
        baseline_context: baseline.map(crate::collected_context::from_capture),
        report,
    })
}

struct CapturedEntries<'a> {
    context: NativeReadContext,
    observations: Vec<&'a OperationalObservation>,
    source: SourceOutcome,
    read_complete: bool,
}

impl<'a> CapturedEntries<'a> {
    fn new(capture: &'a EvidenceCapture) -> Result<Self> {
        let input = CapturedState::new(capture, SnapshotDataset::CacheEntryMemory)?;
        input.database()?;
        let SourceObservation::CacheEntryMemory { focus, .. } = &capture.source().observation else {
            return Err(invalid());
        };
        let mut context = NativeReadContext::from_snapshot(capture.source())?;
        // The canonical source header binds this original focus to the same target/member/process. The provider
        // separately verifies a known original role before its frame and at settlement; unknown stays unknown.
        context.database_writable = focus.context.database_writable;
        let observations = input.rows()?;
        for observation in &observations {
            let OperationalFact::MssqlCacheEntryMemory(entry) = &observation.fact else { return Err(invalid()) };
            if !focus.accepts_entry(entry) {
                return Err(invalid());
            }
        }
        let source = input.outcome(observations.len())?;
        Ok(Self { context, observations, source, read_complete: capture.is_read_complete() })
    }

    fn evidence(&self) -> CacheEntryMemoryEvidence<'_> {
        CacheEntryMemoryEvidence {
            context: &self.context,
            observations: &self.observations,
            source: Some(&self.source),
            read_complete: self.read_complete,
        }
    }
}
