//! Reuse the original native owner, request decoder and SQL guard before reading independent gateway context.
use super::participants::{Check, RequestState};
use super::*;

pub(crate) struct CompilerAdmissionCapture {
    pub owner: crate::local_source::Context,
    pub context: NativeReadContext,
    pub observations: Vec<OperationalObservation>,
    pub result: ActivityCompilationAdmissionFocusResult,
    pub gateway_failure: Option<Error>,
}

impl<R: NativeRead> ActivityReader<R> {
    pub(crate) async fn capture_focused_compiler_admission(
        &mut self,
        focus: &ActivityRequestFocus,
        shared: Option<(&SharedResourceIdentity, &SourcePartition)>,
    ) -> Result<CompilerAdmissionCapture> {
        if !focus.is_compilation_admission_focus()
            || focus.source != culprit_core::source_id::SESSIONS
            || RequestLocator::from_identity(&focus.statement_identity).is_none()
        {
            return Err(invalid_data());
        }
        if focus.database_writable.is_none() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let owner = database_context::initial(self).await?;
        if !owner.is_qualified() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        if owner.activity.major < 13 && owner.activity.edition != 5 {
            return Err(Error::new(
                ErrorCode::Unsupported,
                culprit_core::t!("provider.mssql.compiler_gateways.unavailable_version"),
            ));
        }
        super::memory::focus::matches_owner(focus, &owner)?;
        let target = shared_target(&owner, shared)?;
        let mut observations = Vec::new();
        let membership;
        {
            let session = native_id(&focus.original.key.session.id)?;
            let request = native_id(&focus.original.key.request.as_ref().ok_or_else(invalid_data)?.id)?;
            let mut frame = self
                .read_frame(
                    &super::page_targets::focused_observation(
                        self.options.identity,
                        self.request_page_server_read_capability,
                    ),
                    vec![Parameter::Integer(1), Parameter::Integer(session), Parameter::Integer(request)],
                )
                .await?;
            if !owner.matches_frame(&frame.context) || frame.records.len() > 1 {
                return Err(CollectionStop::SourceChanged.error());
            }
            let current = frame.records.first().map(|record| Check::from_record(record, &frame.context.at));
            membership = match current.as_ref().map(|check| check.observation_state(&focus.original)) {
                Some(RequestState::SameStatement)
                    if frame.records[0]
                        .facts
                        .wait
                        .as_ref()
                        .is_some_and(|wait| wait.class == Some(ActivityWaitClass::CompilationAdmission)) =>
                {
                    CompilationAdmissionMembership::Observed
                }
                Some(RequestState::Unverified | RequestState::Changed) => CompilationAdmissionMembership::Unverified,
                _ => CompilationAdmissionMembership::NotObserved,
            };
            if membership == CompilationAdmissionMembership::Observed {
                self.enrich(
                    &mut frame,
                    &BTreeSet::from([focus.original.key.clone()]),
                    false,
                    self.options.budget.max_sql_bytes,
                )
                .await?;
                let at = frame.context.at.clone();
                let record = frame.records.into_iter().next().ok_or_else(invalid_data)?;
                let waiter = CompilerWaiterObservation { observed_at: at, request: record.into_redacted_observation() };
                observations.push(OperationalObservation {
                    source: crate::sources::COMPILATION_GATEWAYS.name.into(),
                    key: waiter.key()?,
                    fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
                });
            }
        }
        let mut gateway_failure = None;
        if membership == CompilationAdmissionMembership::Observed {
            let limit = (self.options.budget.max_snapshot_rows as usize)
                .saturating_sub(observations.len())
                .min(MAX_FOCUSED_COMPILER_GATEWAYS);
            match crate::compiler_gateways::read(&mut self.io, limit).await {
                Ok(gateways) => observations
                    .extend(gateways.iter().map(|gateway| gateway.observation()).collect::<Result<Vec<_>>>()?),
                Err(error) if stops_remaining_reads(&error) => return Err(error),
                Err(error) => gateway_failure = Some(error),
            }
        }
        let after = database_context::recheck(self, &owner).await?;
        if !owner.same_source(&after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        super::memory::focus::matches_owner(focus, &after)?;
        let mut context = owner.completed(&after)?;
        context.target = target;
        if let Some((_, partition)) = shared {
            context.partition = partition.clone();
        }
        let result = ActivityCompilationAdmissionFocusResult {
            focus: focus.clone(),
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            source: FocusSourceState::Matched,
            membership,
            gateways: observations
                .iter()
                .filter_map(|row| match &row.fact {
                    OperationalFact::CounterSet(counters) => Some(counters.identity.clone()),
                    _ => None,
                })
                .collect(),
        };
        if !result.matches_observations(&observations) {
            return Err(invalid_data());
        }
        Ok(CompilerAdmissionCapture { owner, context, observations, result, gateway_failure })
    }

    pub(crate) async fn verify_compiler_admission(&mut self, owner: &crate::local_source::Context) -> Result<bool> {
        let after = database_context::recheck(self, owner).await?;
        Ok(owner.same_source(&after))
    }
}

fn shared_target(
    owner: &crate::local_source::Context,
    shared: Option<(&SharedResourceIdentity, &SourcePartition)>,
) -> Result<CollectionTarget> {
    let Some((resource, expected)) = shared else { return Ok(owner.activity.database.clone().into()) };
    let instance = owner.instance.as_ref().ok_or_else(|| CollectionStop::SourceChanged.error())?;
    let current = SourcePartition::InstanceLocal {
        member: Some(instance.identity.clone()),
        epoch: Some(instance.capture_epoch()?),
    };
    if instance.identity != *resource || current != *expected {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(CollectionTarget::Shared(resource.clone()))
}

fn native_id(value: &str) -> Result<i64> {
    let id = value.parse::<u32>().map_err(|_| invalid_data())?;
    if id.to_string() != value || id > i32::MAX as u32 {
        return Err(invalid_data());
    }
    Ok(i64::from(id))
}

#[cfg(test)]
pub(crate) mod tests;
