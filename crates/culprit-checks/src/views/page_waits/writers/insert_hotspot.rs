//! A recorded end-of-leaf position and an INSERT access plan permit a candidate, never a cause or owner.
use super::super::leaf_position::LeafPosition;
use culprit_core::{SourceOutcome, SourceState, WaitResourceKind, telemetry::*};

#[derive(Clone, Copy)]
pub(super) struct EndOfLeafInsert {
    leading_identity: Option<bool>,
    sequential_key: Option<bool>,
}

impl EndOfLeafInsert {
    pub(super) fn from_observation(
        observation: &PageWaitWriterObservation,
        source: Option<&SourceOutcome>,
    ) -> Option<Self> {
        if observation.target_state != PageWriterTargetState::CurrentMapped
            || !source.is_some_and(|source| matches!(source.state, SourceState::Collected | SourceState::Partial))
        {
            return None;
        }
        let page = observation.page.as_ref()?;
        let wait = page.observed_wait();
        // MemoryPage is the native provider's typed PAGELATCH classification. Do not reclassify SQL text,
        // infer a wait from index totals, or replace a missing native wait with a page's structure.
        if wait.resource.kind != WaitResourceKind::MemoryPage || wait.native_type.is_none() || page.task_wait.is_some()
        {
            return None;
        }
        let target = page.target.as_ref()?;
        if LeafPosition::from_target(target) != Some(LeafPosition::End) || target.writer_target_key().is_err() {
            return None;
        }
        let point = page.index_operations.as_ref()?;
        let PageIndexOperations::Observed { counters, relation: PageIndexOperationRelation::CheckedTarget, .. } = point
        else {
            return None;
        };
        if !point.matches_target(target)
            || counters.object_type.as_deref() != Some("U")
            || counters.engine_internal != Some(false)
        {
            return None;
        }
        Some(Self {
            leading_identity: counters.leading_key_is_identity,
            sequential_key: counters.optimize_for_sequential_key,
        })
    }

    pub(super) fn leading_key_is_identity(self) -> bool {
        self.leading_identity == Some(true)
    }

    pub(super) fn sequential_key_setting(self) -> Option<bool> {
        self.sequential_key
    }
}
