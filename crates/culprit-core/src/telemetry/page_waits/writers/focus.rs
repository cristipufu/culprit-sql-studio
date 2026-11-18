use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageWaitWriterRequest {
    pub parent: DiagnosticEvidenceRef,
    pub key: ActivityKey,
    pub statement_identity: NativeIdentity,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageWaitWriterFocus {
    pub waiter: ActivityWaitFocus,
    pub page: DiagnosticEvidenceRef,
    pub address: PageWaitAddress,
    pub target: Box<MappedPageTarget>,
    pub candidates: Vec<PageWaitWriterRequest>,
}

impl PageWaitWriterFocus {
    /// This SQL Server question needs a known original database role and one qualified native local owner.
    /// Read-only is a known role; neither role permits mutation or establishes that a plan executed a write.
    pub fn accepts_waiter(waiter: &ActivityWaitFocus) -> bool {
        let participant = waiter.participant();
        waiter.is_valid()
            && waiter.resource.kind == WaitResourceKind::MemoryPage
            && participant.target.engine() == crate::Engine::Mssql
            && matches!(participant.partition, SourcePartition::InstanceLocal { .. })
            && participant.partition.is_qualified()
            && participant.database_writable.is_some()
    }

    pub fn is_valid(&self) -> bool {
        let waiter = self.waiter.participant();
        let mut keys = BTreeSet::new();
        Self::accepts_waiter(&self.waiter)
            && self.page.is_valid()
            && self.page.stream == TelemetryStream::OperationalSamples
            && matches!(self.page.native_key, NativeRecordIdentity::Operational { .. })
            && self.address.is_valid()
            && self.address.matches_header(&self.target.header)
            && self.target.writer_target_key().is_ok()
            && (1..=MAX_PAGE_WAIT_WRITER_CANDIDATES).contains(&self.candidates.len())
            && self.candidates.iter().all(|candidate| {
                candidate.parent.is_valid()
                    && candidate.parent.capture_ref() == waiter.parent.capture_ref()
                    && candidate.parent.stream == TelemetryStream::ActivityObservations
                    && matches!(&candidate.parent.native_key, NativeRecordIdentity::Activity(key) if key == &candidate.key)
                    && candidate.key.is_valid()
                    && candidate.key.request.is_some()
                    && candidate.statement_identity.is_valid()
                    && keys.insert(&candidate.key)
            })
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("page-wait-writer-focus-v1", self))
    }

    /// The frame owns original generations and every selected statement token. Its caller authenticates storage;
    /// a later frame, a partial roster or a copied focus observation cannot substitute for these native rows.
    pub fn matches_activity_frame(&self, frame: &ActivityFrameEvidence<'_>) -> bool {
        let original = self.waiter.participant();
        let Some(waiter) = frame.row(&original.original.key) else { return false };
        self.is_valid()
            && frame.read_complete()
            && frame.context().has_qualified_local_owner()
            && frame.has_session_generation_authority(&original.original.key.session)
            && waiter
                .reference
                .is_some_and(|reference| original.matches_parent(frame.source(), waiter.observation, reference))
            && self.candidates.iter().all(|candidate| {
                frame.row(&candidate.key).is_some_and(|row| {
                    frame.has_session_generation_authority(&candidate.key.session)
                        && row.reference == Some(&candidate.parent)
                        && row.observation.statement_identity.as_ref() == Some(&candidate.statement_identity)
                        && row.observation.scope == ActivityScopeBasis::RequestDatabase
                        && original
                            .target
                            .database()
                            .is_some_and(|database| row.observation.database_context_is_consistent(&database.database))
                })
            })
    }

    /// The caller independently authenticates the page row and every candidate's original Activity receipt.
    pub fn matches_page(
        &self,
        source: &CollectionDescription,
        row: &PageWaitObservation,
        reference: &DiagnosticEvidenceRef,
    ) -> bool {
        self.is_valid()
            && &self.page == reference
            && row.fits_source_scope(source)
            && row.task_wait.is_none()
            && source.target == self.waiter.participant().target
            && source.partition == self.waiter.participant().partition
            && source.captured_range().is_ok_and(|range| range.scope_id == reference.source_scope_id)
            && row.request.key == self.waiter.participant().original.key
            && row.statement_identity.as_ref() == Some(&self.waiter.participant().statement_identity)
            && row.resource == self.waiter.resource
            && row.observed_address == Some(self.address)
            && row.target.as_deref() == Some(self.target.as_ref())
            && matches!(&reference.native_key, NativeRecordIdentity::Operational { source, key } if source == &row.source && row.key().is_ok_and(|expected| *key == expected))
    }

    pub fn accepts_observation(&self, context: &NativeReadContext, row: &PageWaitWriterObservation) -> bool {
        if !self.is_valid()
            || !row.is_valid()
            || !context.has_qualified_local_owner()
            || context.target != self.waiter.participant().target
            || context.partition != self.waiter.participant().partition
            || context.database_writable != self.waiter.participant().database_writable
            || self.key().ok().as_deref() != Some(row.focus_key.as_str())
            || ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_none_or(|bounds| !bounds.contains(&row.observed_from) || !bounds.contains(&row.observed_until))
            || ObservationBounds::new(&self.waiter.participant().original_observed_at, &row.observed_from).is_none()
            || row.candidates.len() != self.candidates.len()
            || row.page.as_ref().is_some_and(|page| {
                !self
                    .waiter
                    .participant()
                    .target
                    .database()
                    .is_some_and(|database| page.fits_database_lookup(&database.database))
            })
        {
            return false;
        }
        if row.target_state == PageWriterTargetState::CurrentMapped {
            let Some(page) = &row.page else { return false };
            if page.request.key != self.waiter.participant().original.key
                || page.resource != self.waiter.resource
                || page.statement_identity.as_ref() != Some(&self.waiter.participant().statement_identity)
                || page.task_wait.is_some()
                || page.resolution != PageWaitResolution::ObservedAtBothBounds
                || page.target.as_ref().and_then(|target| target.writer_target_key().ok())
                    != self.target.writer_target_key().ok()
                || [&page.before_check, &page.after_check].into_iter().any(|check| {
                    !check.as_ref().is_some_and(|check| {
                        check.request == PageWaitRequestState::SameStatement
                            && check.wait == PageWaitRelation::OriginalObserved
                    })
                })
            {
                return false;
            }
        }
        self.candidates.iter().zip(&row.candidates).all(|(original, current)| {
            current.parent == original.parent
                && current.request.as_ref().is_none_or(|request| {
                    request.key == original.key
                        && request.statement_identity.as_ref() == Some(&original.statement_identity)
                        && self.waiter.participant().target.database().is_some_and(|database| {
                            request.scope == ActivityScopeBasis::RequestDatabase
                                && request.database_context_is_consistent(&database.database)
                        })
                })
                && current.binding.as_ref().is_none_or(|proof| proof.matches(&self.target, current))
        })
    }
}
