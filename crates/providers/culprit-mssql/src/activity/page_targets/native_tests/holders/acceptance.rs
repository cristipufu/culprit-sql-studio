//! Native compatibility requires actual task evidence; only a separately validated positive qualifies a holder.
use super::*;
use culprit_core::SourceState;

#[derive(Default)]
pub(super) struct NativeHolderEvidence {
    native_task_waits: usize,
    qualified_points: usize,
}

impl NativeHolderEvidence {
    pub(super) fn observe_page(
        &mut self,
        page: &PageWaitObservation,
        original_requests: &[ActivityRequestSelector],
    ) -> Result<()> {
        if !original_requests.iter().any(|original| {
            original.key == page.request.key
                && page.request.statement_identity.as_ref() == Some(&original.statement_identity)
        }) {
            return Err(invalid("native page row did not retain a request from the exact original roster"));
        }
        if page.task_wait.as_ref().is_some_and(|task| task.resource.kind == culprit_core::WaitResourceKind::MemoryPage)
        {
            self.native_task_waits += 1;
        }
        Ok(())
    }

    pub(super) fn observe_qualified_points(&mut self, points: usize) {
        self.qualified_points += points;
    }

    pub(super) fn native_task_waits(&self) -> usize {
        self.native_task_waits
    }

    pub(super) fn has_qualified_holder(&self) -> bool {
        self.qualified_points > 0
    }

    pub(super) fn require_compatibility(&self) -> Result<()> {
        if self.native_task_waits == 0 {
            return Err(invalid(
                "finite owned native page workload exposed no original native page-latch task evidence",
            ));
        }
        Ok(())
    }

    pub(super) fn require_qualified_holder(&self) -> Result<()> {
        self.require_compatibility()?;
        if !self.has_qualified_holder() {
            return Err(invalid(
                "finite owned native page workload did not expose a qualified blocking-task/request relationship; no positive proof",
            ));
        }
        Ok(())
    }
}

pub(super) fn require_readable_capture(capture: &Captured) -> Result<()> {
    let metrics = capture.manifest.read_metrics.as_ref().ok_or_else(|| invalid("native source accounting absent"))?;
    // OwnedCaptures already authenticates canonical persisted rows, completion and confirmed shutdown. This
    // scenario additionally refuses swallowed source errors; a truthful limitation is not compatibility proof.
    require_successful_reads(metrics)?;
    require_readable_source(capture.source()?)
}

fn require_successful_reads(metrics: &ReadWorkMetrics) -> Result<()> {
    if metrics.templates.iter().any(|template| template.failures != 0) {
        return Err(invalid("owned native compatibility capture included a failed source read"));
    }
    Ok(())
}

fn require_readable_source(source: &CollectionDescription) -> Result<()> {
    let coverage = match &source.observation {
        SourceObservation::Activity(activity) => std::slice::from_ref(&activity.coverage),
        observation => observation
            .snapshot()
            .map(|snapshot| snapshot.sources.as_slice())
            .ok_or_else(|| invalid("owned native compatibility capture omitted its declared source coverage"))?,
    };
    if coverage.is_empty()
        || coverage.iter().any(|source| {
            matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable)
                || source.error_code.is_some()
                || source.stopped.is_some()
        })
    {
        return Err(invalid("owned native compatibility capture did not read its declared source successfully"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
