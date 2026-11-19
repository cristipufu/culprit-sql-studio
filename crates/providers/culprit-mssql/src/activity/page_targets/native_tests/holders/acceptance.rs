//! Native compatibility requires actual task evidence; only a separately validated positive qualifies a holder.
use super::*;

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

pub(super) use super::super::durable_capture::{
    require_readable_capture, require_readable_source, require_successful_reads,
};

#[cfg(test)]
mod tests;
