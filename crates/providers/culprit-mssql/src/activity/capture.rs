//! One held authority sequence surrounds the same census frame and any explicitly eligible SQL enrichment.
use super::{
    ActivityBaseline, ActivityFrame, ActivityReader,
    database_context::{self, Context},
};
use culprit_core::{ErrorCode, MeasurementScope, Result, telemetry::*};

pub(crate) struct PendingActivityFrame {
    pub baseline: ActivityBaseline,
    before: AuthorityRead,
    authority: ActivityAuthorityPolicy,
}
pub(crate) struct CompletedActivityFrame {
    pub description: CollectionDescription,
    pub frame: ActivityFrame,
    pub action_context: Option<NativeReadContext>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityAuthorityPolicy {
    RetainPartialBaseline,
    AdministrativeCandidates,
    MetadataOnly,
}
enum MetadataProjection {
    Standard,
    Census,
}

impl PendingActivityFrame {
    pub(crate) async fn begin<R: crate::read::NativeRead>(
        reader: &mut ActivityReader<R>,
        authority: ActivityAuthorityPolicy,
    ) -> Result<Self> {
        Self::begin_with_projection(reader, authority, MetadataProjection::Standard).await
    }
    pub(crate) async fn begin_census<R: crate::read::NativeRead>(reader: &mut ActivityReader<R>) -> Result<Self> {
        Self::begin_with_projection(reader, ActivityAuthorityPolicy::RetainPartialBaseline, MetadataProjection::Census)
            .await
    }
    async fn begin_with_projection<R: crate::read::NativeRead>(
        reader: &mut ActivityReader<R>,
        authority: ActivityAuthorityPolicy,
        projection: MetadataProjection,
    ) -> Result<Self> {
        let before = if authority != ActivityAuthorityPolicy::MetadataOnly {
            AuthorityRead::admit(database_context::initial(reader).await, authority)?
        } else {
            AuthorityRead::unobserved()
        };
        let baseline = match projection {
            MetadataProjection::Standard => reader.observe_baseline().await?,
            MetadataProjection::Census => reader.observe_census_baseline().await?,
        };
        Ok(Self { baseline, before, authority })
    }
    pub(crate) fn require_complete(&self) -> Result<()> {
        if let Some(error) = &self.baseline.restriction {
            return Err(error.clone());
        }
        if let Some(stop) = self.baseline.coverage.stopped {
            return Err(stop.error());
        }
        Ok(())
    }
    pub(crate) async fn finish<R: crate::read::NativeRead>(
        self,
        reader: &mut ActivityReader<R>,
    ) -> Result<CompletedActivityFrame> {
        let Self { mut baseline, before, authority } = self;
        let after = if authority == ActivityAuthorityPolicy::RetainPartialBaseline || before.context.is_some() {
            AuthorityRead::admit(
                match &before.context {
                    Some(context) => database_context::recheck(reader, context).await,
                    None => database_context::initial(reader).await,
                },
                authority,
            )?
        } else {
            AuthorityRead::unobserved()
        };
        // Administrative candidates preserve the pre-existing stronger refusal of changed observed authority,
        // including when that authority cannot establish a qualified native member.
        let action_context = if authority == ActivityAuthorityPolicy::AdministrativeCandidates {
            match (&before.context, &after.context) {
                (Some(before), Some(after)) => {
                    let context = before.completed(after)?;
                    (before.is_qualified() && after.is_qualified()).then_some(context)
                }
                _ => None,
            }
        } else {
            None
        };
        if before.context.is_none() || after.context.is_none() {
            // Optional columns were admitted by the initial held profile. A missing final authority cannot
            // attest that profile; retain the independent activity frame and its explicit coverage gap.
            for record in &mut baseline.frame.records {
                if let Some(counters) = &mut record.facts.request_counters {
                    counters.page_server_reads = None;
                }
            }
        }
        for context in [&before.context, &after.context].into_iter().flatten() {
            if !context.matches_frame(&baseline.frame.context) {
                return Err(CollectionStop::SourceChanged.error());
            }
        }
        if let (Some(before), Some(after)) = (&before.context, &after.context)
            && before.connection_changed(after)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let qualified = match (&before.context, &after.context) {
            (Some(before), Some(after)) if before.is_qualified() && after.is_qualified() => {
                if !before.same_source(after) {
                    return Err(CollectionStop::SourceChanged.error());
                }
                true
            }
            _ => false,
        };
        let database_wide =
            [&before.context, &after.context].into_iter().flatten().all(|context| context.database_wide);
        if !database_wide || !qualified {
            baseline.coverage.state = culprit_core::SourceState::Partial;
            baseline.coverage.complete_scope = false;
            baseline.coverage.error_code = before.error_code.or(after.error_code).or(baseline.coverage.error_code);
            if !database_wide {
                baseline.coverage.error_code = Some(ErrorCode::PermissionDenied);
            }
            baseline.coverage.stopped.get_or_insert(CollectionStop::SourceUnavailable);
        }
        let observed_from = before
            .context
            .as_ref()
            .map_or_else(|| baseline.frame.context.at.clone(), |context| context.activity.at.clone());
        let observed_until = after
            .context
            .as_ref()
            .map_or_else(|| baseline.frame.context.at.clone(), |context| context.activity.at.clone());
        let mut context = match &before.context {
            Some(before) => before.capture_context()?,
            None => baseline.frame.context.clone(),
        };
        if !qualified {
            context.epoch = None;
        }
        let at = baseline.frame.context.at.clone();
        let database_writable = baseline.frame.context.writable;
        let member = before.context.as_ref().and_then(Context::source_instance);
        let partition = SourcePartition::InstanceLocal { member, epoch: context.epoch.clone() };
        let observations = baseline.frame.records.len() as u32;
        let activity = ActivitySnapshotDescription {
            epoch: context.epoch,
            database_writable,
            observed_from,
            observed_until,
            at,
            scope: MeasurementScope::Database,
            population: ActivityPopulation::UserSessionsCurrentRequestsAndBlockers,
            native_rows: baseline.native_rows,
            observations,
            coverage: baseline.coverage,
        };
        if !activity.is_valid() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let description = CollectionDescription {
            target: context.database.into(),
            partition,
            source: culprit_core::source_id::SESSIONS.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(activity),
        };
        Ok(CompletedActivityFrame { description, frame: baseline.frame, action_context })
    }
}

/// A completed metadata authority failure cannot erase the independent native actor frame. Terminal
/// ownership/budget failures still stop all follow-up work and require the enclosing source to close.
struct AuthorityRead {
    context: Option<Context>,
    error_code: Option<ErrorCode>,
}
impl AuthorityRead {
    fn unobserved() -> Self {
        Self { context: None, error_code: None }
    }
    fn admit(result: Result<Context>, policy: ActivityAuthorityPolicy) -> Result<Self> {
        match result {
            Ok(context) => Ok(Self { error_code: context.qualification_error, context: Some(context) }),
            Err(error) if crate::read::stops_remaining_reads(&error) => Err(error),
            Err(error)
                if policy == ActivityAuthorityPolicy::AdministrativeCandidates
                    && !matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
                    && !matches!(
                        error.detail,
                        Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceUnavailable))
                    ) =>
            {
                Err(error)
            }
            Err(error) => Ok(Self { context: None, error_code: Some(error.code) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_optional_authority_failure_retains_a_partial_baseline_but_not_an_administrative_identity() {
        let denied = culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture denied");
        for policy in
            [ActivityAuthorityPolicy::RetainPartialBaseline, ActivityAuthorityPolicy::AdministrativeCandidates]
        {
            let retained = AuthorityRead::admit(Err(denied.clone()), policy).unwrap();
            assert!(retained.context.is_none());
            assert_eq!(retained.error_code, Some(ErrorCode::PermissionDenied));
        }
        let malformed = CollectionStop::InvalidPage.error();
        assert!(AuthorityRead::admit(Err(malformed.clone()), ActivityAuthorityPolicy::RetainPartialBaseline).is_ok());
        let refused = AuthorityRead::admit(Err(malformed.clone()), ActivityAuthorityPolicy::AdministrativeCandidates)
            .err()
            .unwrap();
        assert_eq!((refused.code, refused.detail), (malformed.code, malformed.detail));
    }

    #[test]
    fn native_uncertainty_budget_cancellation_and_source_change_stop_both_consumers_without_an_authority_retry() {
        for stop in [
            CollectionStop::Cancelled,
            CollectionStop::TimedOut,
            CollectionStop::ByteLimit,
            CollectionStop::SourceChanged,
            CollectionStop::CancellationUnconfirmed,
        ] {
            for policy in
                [ActivityAuthorityPolicy::RetainPartialBaseline, ActivityAuthorityPolicy::AdministrativeCandidates]
            {
                let refused = AuthorityRead::admit(Err(stop.error()), policy).err().unwrap();
                assert_eq!(refused.detail, Some(culprit_core::ErrorDetail::CollectionStopped(stop)));
            }
        }
    }
}
