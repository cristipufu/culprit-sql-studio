use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageTaskHolderFocus {
    /// The original request owns its generations. A parallel task need not share its coordinator's wait type.
    pub participant: ActivityRequestFocus,
    pub page: DiagnosticEvidenceRef,
    pub address: PageWaitAddress,
    pub task: MssqlPageTaskWait,
    pub target: Box<MappedPageTarget>,
    pub original_observed_until: String,
}

impl PageTaskHolderFocus {
    pub fn is_valid(&self) -> bool {
        self.participant.is_valid()
            && self.participant.target.engine() == crate::Engine::Mssql
            && self.participant.original.scope == ActivityScopeBasis::RequestDatabase
            && self.participant.source == crate::source_id::SESSIONS
            && matches!(self.participant.partition, SourcePartition::InstanceLocal { .. })
            && self.participant.partition.is_qualified()
            && self.participant.database_writable.is_some()
            && self.page.is_valid()
            && self.page.stream == TelemetryStream::OperationalSamples
            && matches!(self.page.native_key, NativeRecordIdentity::Operational { .. })
            && self.address.is_valid()
            && self.address.matches_header(&self.target.header)
            && self.target.writer_target_key().is_ok()
            && self.task.is_valid()
            && self.task.resource.kind == WaitResourceKind::MemoryPage
            && PageWaitAddress::parse_sql_server_resource(&self.task.resource.native.id) == Some(self.address)
            && self.task.matches_request(&self.participant.original)
            && ObservationBounds::new(&self.participant.original_observed_at, &self.task.observed_at).is_some()
            && ObservationBounds::new(&self.task.observed_at, &self.original_observed_until).is_some()
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&("page-task-holder-focus-v1", self))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }

    /// Storage authentication is the caller's responsibility. This comparison never replaces the original
    /// task or request with a later sample or assumes that a reused task address has its own generation.
    pub fn matches_page(
        &self,
        source: &CollectionDescription,
        row: &PageWaitObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> bool {
        self.is_valid()
            && &self.page == reference
            && row.fits_source_scope(source)
            && source.target == self.participant.target
            && source.partition == self.participant.partition
            && match &source.observation {
                SourceObservation::PageWaitTargets { original_activity: Some(activity), .. } => {
                    *activity == self.participant.parent.capture_ref()
                }
                _ => true,
            }
            && NativeReadContext::from_snapshot(source)
                .is_ok_and(|context| context.database_writable == self.participant.database_writable)
            && source.captured_range().is_ok_and(|range| range.scope_id == reference.source_scope_id)
            && row.request.key == self.participant.original.key
            && row.statement_identity.as_ref() == Some(&self.participant.statement_identity)
            && row.task_wait.as_ref() == Some(&self.task)
            && row.observed_until == self.original_observed_until
            && row.observed_address == Some(self.address)
            && row.target.as_ref() == Some(&self.target)
            && matches!(&reference.native_key, NativeRecordIdentity::Operational { source, key }
                if source == &row.source && row.key().is_ok_and(|expected| *key == expected))
    }

    pub fn matches_activity_frame(&self, frame: &ActivityFrameEvidence<'_>) -> bool {
        let Some(row) = frame.row(&self.participant.original.key) else { return false };
        self.is_valid()
            && frame.read_complete()
            && frame.context().has_qualified_local_owner()
            && row
                .reference
                .is_some_and(|reference| self.participant.matches_parent(frame.source(), row.observation, reference))
    }

    pub fn accepts_observation(&self, context: &NativeReadContext, row: &PageTaskHolderObservation) -> bool {
        let Some(bounds) = ObservationBounds::new(&context.observed_from, &context.observed_until) else {
            return false;
        };
        self.is_valid()
            && row.is_valid()
            && context.has_qualified_local_owner()
            && context.target == self.participant.target
            && context.partition == self.participant.partition
            && context.database_writable == self.participant.database_writable
            && row.before.database_writable == context.database_writable
            && row.after.database_writable == context.database_writable
            && self.key().ok().as_deref() == Some(row.focus_key.as_str())
            && bounds.contains(&row.observed_from)
            && bounds.contains(&row.observed_until)
            && ObservationBounds::new(&self.original_observed_until, &row.observed_from).is_some()
            && self.matches_current_page(row)
            && (row.target_state != PageWriterTargetState::ChangedTarget
                || row.page.as_ref().and_then(|page| page.target.as_ref()).is_some_and(|target| {
                    target.writer_target_key().ok().is_some_and(|key| Some(key) != self.target.writer_target_key().ok())
                })
                || row.final_target.as_ref().is_some_and(|check| {
                    check
                        .target
                        .writer_target_key()
                        .ok()
                        .is_some_and(|key| Some(key) != self.target.writer_target_key().ok())
                }))
            && self.matches_current_task(row.after_task.as_ref(), &row.after)
            && [&row.before, &row.after].into_iter().all(|point| {
                point.holder.request.as_ref().is_none_or(|request| {
                    self.participant
                        .target
                        .database()
                        .is_some_and(|database| request.database_context_is_consistent(&database.database))
                })
            })
    }

    fn matches_current_page(&self, row: &PageTaskHolderObservation) -> bool {
        let Some(page) = &row.page else {
            return row.target_state != PageWriterTargetState::CurrentMapped;
        };
        page.request.key == self.participant.original.key
            && self.participant.target.database().is_some_and(|database| page.fits_database_lookup(&database.database))
            && page.resource.same_sql_server_page(&self.task.resource)
            && page.observed_address == Some(self.address)
            && (row.before.request != PageWaitRequestState::SameStatement
                || page.request.statement_identity.as_ref() == Some(&self.participant.statement_identity))
            && self.matches_current_task(page.task_wait.as_ref(), &row.before)
            && (row.target_state != PageWriterTargetState::CurrentMapped
                || page.resolution == PageWaitResolution::ObservedAtBothBounds
                    && page.target.as_ref().and_then(|target| target.writer_target_key().ok())
                        == self.target.writer_target_key().ok())
    }

    fn matches_current_task(&self, task: Option<&MssqlPageTaskWait>, point: &MssqlPageTaskHolderPoint) -> bool {
        task.is_none_or(|task| {
            task.matches_request(&self.participant.original)
                && (point.wait != PageWaitRelation::OriginalObserved
                    || task.task_address == self.task.task_address
                        && task.execution_context_id == self.task.execution_context_id
                        && task.wait_type == self.task.wait_type
                        && task.resource.same_sql_server_page(&self.task.resource))
        })
    }
}
