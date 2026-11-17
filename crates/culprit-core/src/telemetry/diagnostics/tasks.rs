//! A parallel task address is a point locator, never a generation or a lifecycle/absence proof.
use super::*;

impl DiagnosticSignal {
    pub(crate) fn is_task_point(&self) -> bool {
        matches!(self, Self::PageTaskWait { .. })
    }

    pub(super) fn is_valid_task_wait(&self) -> bool {
        let Self::PageTaskWait { key, statement_identity, parent, page, task, .. } = self else { return false };
        key.request.is_some()
            && statement_identity.is_valid()
            && parent.is_valid()
            && page.is_valid()
            && matches!(&parent.native_key, NativeRecordIdentity::Activity(original) if original == key)
            && page.stream == TelemetryStream::OperationalSamples
            && matches!(&page.native_key, NativeRecordIdentity::Operational { .. })
            && task.is_valid()
            && task.resource.kind == crate::WaitResourceKind::MemoryPage
            && task.matches_activity_key(key)
    }

    /// Original prerequisites remain immutable revision inputs before any investigation result exists.
    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        let (parent, page) = match self {
            Self::PageTaskWait { parent, page, .. } => (Some(parent.as_ref()), Some(page.as_ref())),
            _ => (None, None),
        };
        parent.into_iter().chain(page)
    }
}

impl DiagnosticEpisode {
    pub(super) fn task_point_is_consistent(&self) -> bool {
        let DiagnosticSignal::PageTaskWait { page, task, .. } = &self.signal else { return true };
        self.scope.target.engine() == crate::Engine::Mssql
            && self.scope.target.database().is_some()
            && matches!(self.scope.partition, SourcePartition::InstanceLocal { .. })
            && self.scope.partition.is_qualified()
            && self.scope.native_epoch.is_some()
            && self.scope.source == crate::source_id::SESSIONS
            && self.scope.measurement_scope == crate::MeasurementScope::Database
            && self.first == **page
            && self.latest == **page
            && self.peak.is_none()
            && self.first_seen_at == task.observed_at
            && self.last_seen_at == task.observed_at
            && self.lifecycle == DiagnosticLifecycle::Uncertain
            && self.recovery.is_none()
    }

    /// The task keeps its own original receipt and resource even if its coordinator waited on something else.
    pub fn matches_page_task_holder_focus(&self, focus: &PageTaskHolderFocus) -> bool {
        let DiagnosticSignal::PageTaskWait { key, statement_identity, parent, page, task, database_writable } =
            &self.signal
        else {
            return false;
        };
        let original = &focus.participant;
        self.is_valid()
            && focus.is_valid()
            && key == &original.original.key
            && statement_identity == &original.statement_identity
            && parent.as_ref() == &original.parent
            && page.as_ref() == &focus.page
            && task.as_ref() == &focus.task
            && Some(*database_writable) == original.database_writable
            && self.scope.source == original.source
            && self.scope.native_epoch.as_deref() == Some(original.epoch.as_str())
            && self.scope.partition == original.partition
            && self.scope.target == original.target
    }
}
