//! Native capability, body and final checks share the caller's single ReadSession.
use super::*;
pub(super) enum ReadSubject<'a> {
    Selected(&'a CollectionSubject),
    Connected,
}
pub(super) async fn capture<R: NativeRead>(
    io: &mut R,
    subject: ReadSubject<'_>,
    budget: &CollectionBudget,
    host: &str,
    port: u16,
) -> Result<MssqlAgentJobsSnapshot> {
    let platform = crate::instance::observe(io).await?;
    let generation = io.connection_generation();
    if platform.edition == 5 {
        if matches!(subject, ReadSubject::Selected(CollectionSubject::Shared { .. })) {
            return Err(unsupported());
        }
        let read = azure_not_applicable(io, host, port, platform).await?;
        ensure_generation(io, generation)?;
        return Ok(read);
    }
    if matches!(subject, ReadSubject::Selected(CollectionSubject::Database)) {
        return Err(unsupported());
    }
    let before = platform.context(io).await?.ok_or_else(unsupported)?;
    let from = platform.observed_at;
    let target = CollectionTarget::Shared(before.identity.clone());
    if matches!(subject,ReadSubject::Selected(expected) if !expected.accepts(&target)) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let edition = u32::try_from(before.engine_edition()).map_err(|_| invalid_data())?;
    if edition == 4 {
        let (after, until) = crate::instance::read(io).await?;
        ensure_generation(io, generation)?;
        if before != after {
            return Err(CollectionStop::SourceChanged.error());
        }
        return not_applicable(
            target,
            SourcePartition::InstanceLocal {
                member: Some(before.identity.clone()),
                epoch: Some(before.capture_epoch()?),
            },
            from,
            until,
            edition,
            before.major,
        );
    }
    if ![2, 3, 8].contains(&edition) {
        return Err(unsupported());
    }
    crate::resources::admit_instance_resources(io).await.map_err(ReadError::into_error)?;
    let initial = layout::read(io, edition, before.major).await;
    let (mut metadata, initial_error) = match initial {
        Ok(metadata) => (metadata, None),
        Err(error) if terminal(&error) => return Err(error),
        Err(error) => {
            let access = if error.code == ErrorCode::PermissionDenied {
                MssqlAgentAccess::PermissionDenied
            } else {
                MssqlAgentAccess::Unreported
            };
            (layout::unavailable(edition, before.major, access), Some(error.code))
        }
    };
    crate::resources::admit_instance_resources(io).await.map_err(ReadError::into_error)?;
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    let context_before =
        read_context(io, &metadata, MssqlAgentContextBound::Before, &mut rows, budget.max_snapshot_rows).await?;
    let final_context_rows =
        u32::from(metadata.available(MssqlAgentObject::Sessions) || metadata.available(MssqlAgentObject::Services));
    for family in [Family::Jobs, Family::Steps, Family::History] {
        let (outcome, values) = read_family(
            io,
            &metadata,
            family,
            context_before.point(),
            budget.max_snapshot_rows.saturating_sub(rows.len() as u32).saturating_sub(final_context_rows),
        )
        .await?;
        if family == Family::History {
            metadata.history_limit = family
                .limit()
                .min(budget.max_snapshot_rows.saturating_sub(rows.len() as u32).saturating_sub(final_context_rows))
                .max(1);
        }
        sources.push(outcome);
        rows.extend(values);
    }
    match initial_error {
        Some(error_code) => metadata.recheck = MssqlAgentMetadataRecheck::Unavailable { error_code },
        None => {
            metadata.recheck = match layout::read(io, edition, before.major).await {
                Ok(final_metadata) if same_layout(&final_metadata, &metadata) => MssqlAgentMetadataRecheck::Unchanged,
                Ok(_) => MssqlAgentMetadataRecheck::Changed,
                Err(error) if terminal(&error) => return Err(error),
                Err(error) => MssqlAgentMetadataRecheck::Unavailable { error_code: error.code },
            };
        }
    }
    let context_after = if metadata.recheck == MssqlAgentMetadataRecheck::Unchanged {
        read_context(io, &metadata, MssqlAgentContextBound::After, &mut rows, budget.max_snapshot_rows).await?
    } else {
        ContextRead::NotRead
    };
    let (after, until) = crate::instance::read(io).await?;
    ensure_generation(io, generation)?;
    if before != after {
        return Err(CollectionStop::SourceChanged.error());
    }
    sources.push(context_coverage(&metadata, &context_before, &context_after));
    if metadata.recheck != MssqlAgentMetadataRecheck::Unchanged {
        for source in &mut sources {
            if matches!(source.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty) {
                source.state = culprit_core::SourceState::Partial;
                source.complete_scope = false;
            }
        }
    }
    let read = MssqlAgentJobsSnapshot {
        context: NativeReadContext {
            target,
            partition: SourcePartition::InstanceLocal {
                member: Some(before.identity.clone()),
                epoch: Some(before.capture_epoch()?),
            },
            database_writable: None,
            observed_from: from,
            observed_until: until,
        },
        metadata,
        sources,
        observations: rows,
    };
    read.is_valid_read(true).then_some(read).ok_or_else(invalid_data)
}

fn same_layout(before: &MssqlAgentJobsMetadata, after: &MssqlAgentJobsMetadata) -> bool {
    before.engine_edition == after.engine_edition
        && before.major_version == after.major_version
        && before.availability == after.availability
        && before.visibility == after.visibility
        && before.capabilities == after.capabilities
}

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Jobs,
    Steps,
    History,
}
impl Family {
    fn object(self) -> MssqlAgentObject {
        match self {
            Self::Jobs => MssqlAgentObject::Jobs,
            Self::Steps => MssqlAgentObject::Steps,
            Self::History => MssqlAgentObject::History,
        }
    }
    fn source(self) -> &'static str {
        match self {
            Self::Jobs => MSSQL_AGENT_JOBS_SOURCE,
            Self::Steps => MSSQL_AGENT_STEPS_SOURCE,
            Self::History => MSSQL_AGENT_HISTORY_SOURCE,
        }
    }
    fn limit(self) -> u32 {
        match self {
            Self::Jobs => MAX_JOB_ROWS,
            Self::Steps => MAX_STEP_ROWS,
            Self::History => MAX_HISTORY_ROWS,
        }
    }
    fn decode(self, row: &Rec) -> Result<OperationalFact> {
        match self {
            Self::Jobs => decode::job(row),
            Self::Steps => decode::step(row),
            Self::History => decode::history(row),
        }
    }
}
async fn read_family<R: NativeRead>(
    io: &mut R,
    metadata: &MssqlAgentJobsMetadata,
    family: Family,
    before: Option<&MssqlAgentContext>,
    remaining: u32,
) -> Result<(StateSourceCoverage, Vec<OperationalObservation>)> {
    if !metadata.available(family.object()) {
        return Ok((unavailable_source(family.source(), metadata.capability(family.object()).unwrap().access), vec![]));
    }
    if remaining == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let limit = remaining.min(family.limit());
    let session =
        before.and_then(|point| point.session.as_ref()).filter(|_| metadata.available(MssqlAgentObject::Activity));
    let query = match family {
        Family::Jobs => sql::jobs(session.is_some()),
        Family::Steps => sql::STEPS.into(),
        Family::History => sql::HISTORY.into(),
    };
    let mut parameters = vec![Parameter::Integer(i64::from(limit) + 1)];
    if family == Family::Jobs
        && let Some(session) = session
    {
        parameters.push(Parameter::Integer(session.session_id.parse().map_err(|_| invalid_data())?));
    }
    let result =
        io.query(&query, parameters, limit as usize + 1).await.map_err(ReadError::into_error).and_then(|rows| {
            let limited = rows.len() > limit as usize;
            let values = rows
                .into_iter()
                .take(limit as usize)
                .map(|row| {
                    let fact = family.decode(&row)?;
                    if let OperationalFact::MssqlAgentJob(point) = &fact {
                        let matches_selected_session = point.activity.as_ref().is_none_or(|actual| {
                            session.is_some_and(|expected| expected.session_id == actual.agent_session_id)
                        });
                        if !matches_selected_session {
                            return Err(CollectionStop::SourceChanged.error());
                        }
                    }
                    let key = match &fact {
                        OperationalFact::MssqlAgentJob(point) => point.key(),
                        OperationalFact::MssqlAgentStep(point) => point.key(),
                        OperationalFact::MssqlAgentHistory(point) => point.key(),
                        _ => return Err(invalid_data()),
                    }?;
                    Ok(OperationalObservation { source: family.source().into(), key, fact })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((limited, values))
        });
    match result {
        Ok((limited, values)) => {
            let complete =
                metadata.visibility == MssqlAgentVisibility::Sysadmin && !limited && family != Family::History;
            Ok((StateSourceCoverage::observed(family.source().into(), values.len() as u32, complete), values))
        }
        Err(error) if terminal(&error) => Err(error),
        Err(error) => Ok((
            StateSourceCoverage::unavailable(
                family.source().into(),
                error.code,
                Some(CollectionStop::from_error(&error)),
            ),
            vec![],
        )),
    }
}
enum ContextRead {
    NotRead,
    Failed(ErrorCode),
    Observed(Box<MssqlAgentContext>),
}
impl ContextRead {
    fn point(&self) -> Option<&MssqlAgentContext> {
        if let Self::Observed(point) = self { Some(point) } else { None }
    }
    fn error_code(&self) -> Option<ErrorCode> {
        if let Self::Failed(code) = self { Some(*code) } else { None }
    }
}
async fn read_context<R: NativeRead>(
    io: &mut R,
    metadata: &MssqlAgentJobsMetadata,
    bound: MssqlAgentContextBound,
    rows: &mut Vec<OperationalObservation>,
    limit: u32,
) -> Result<ContextRead> {
    if !metadata.available(MssqlAgentObject::Sessions) && !metadata.available(MssqlAgentObject::Services) {
        return Ok(ContextRead::NotRead);
    }
    if rows.len() >= limit as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    let native =
        io.query(&sql::context(metadata), vec![], MAX_MSSQL_AGENT_SERVICES).await.map_err(ReadError::into_error);
    let point = match native.and_then(|rows| decode::context(&rows, bound)) {
        Ok(point) => point,
        Err(error) if terminal(&error) => return Err(error),
        Err(error) => return Ok(ContextRead::Failed(error.code)),
    };
    rows.push(OperationalObservation {
        source: MSSQL_AGENT_CONTEXT_SOURCE.into(),
        key: point.key()?,
        fact: OperationalFact::MssqlAgentContext(Box::new(point.clone())),
    });
    Ok(ContextRead::Observed(Box::new(point)))
}
fn context_coverage(
    metadata: &MssqlAgentJobsMetadata,
    before: &ContextRead,
    after: &ContextRead,
) -> StateSourceCoverage {
    let count = u32::from(before.point().is_some()) + u32::from(after.point().is_some());
    let error = before.error_code().or_else(|| after.error_code());
    if count == 0 {
        let denied = metadata
            .capability(MssqlAgentObject::Sessions)
            .into_iter()
            .chain(metadata.capability(MssqlAgentObject::Services))
            .any(|capability| capability.access == MssqlAgentAccess::PermissionDenied);
        return StateSourceCoverage::unavailable(
            MSSQL_AGENT_CONTEXT_SOURCE.into(),
            error.unwrap_or(if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported }),
            None,
        );
    }
    let complete = metadata.visibility == MssqlAgentVisibility::Sysadmin
        && metadata.available(MssqlAgentObject::Sessions)
        && metadata.available(MssqlAgentObject::Services)
        && before.point().zip(after.point()).is_some_and(|(before, after)| {
            !before.services_limited
                && !after.services_limited
                && !before.services.is_empty()
                && !after.services.is_empty()
                && before.session.is_some()
                && before.session == after.session
        })
        && metadata.recheck == MssqlAgentMetadataRecheck::Unchanged;
    let mut outcome = StateSourceCoverage::observed(MSSQL_AGENT_CONTEXT_SOURCE.into(), count, complete);
    outcome.error_code = error;
    outcome
}
fn unavailable_source(source: &str, access: MssqlAgentAccess) -> StateSourceCoverage {
    StateSourceCoverage::unavailable(
        source.into(),
        if access == MssqlAgentAccess::PermissionDenied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported },
        None,
    )
}
fn terminal(error: &Error) -> bool {
    stops_remaining_reads(error) || CollectionStop::from_error(error) == CollectionStop::RowLimit
}
async fn azure_not_applicable<R: NativeRead>(
    io: &mut R,
    host: &str,
    port: u16,
    before: crate::instance::Observation,
) -> Result<MssqlAgentJobsSnapshot> {
    let (database, physical) = crate::telemetry::query_history_identity(io, host, port).await?;
    let (after_database, after_physical) = crate::telemetry::query_history_identity(io, host, port).await?;
    let after = crate::instance::observe(io).await?;
    if before.edition != after.edition
        || before.major != after.major
        || database != after_database
        || physical != after_physical
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    not_applicable(database.into(), SourcePartition::Target, before.observed_at, after.observed_at, 5, before.major)
}
fn not_applicable(
    target: CollectionTarget,
    partition: SourcePartition,
    from: String,
    until: String,
    edition: u32,
    major: u32,
) -> Result<MssqlAgentJobsSnapshot> {
    let read = MssqlAgentJobsSnapshot {
        context: NativeReadContext {
            target,
            partition,
            database_writable: None,
            observed_from: from,
            observed_until: until,
        },
        metadata: layout::unavailable(edition, major, MssqlAgentAccess::NotApplicable),
        sources: MSSQL_AGENT_SOURCES
            .into_iter()
            .map(|source| StateSourceCoverage::not_applicable(source.into()))
            .collect(),
        observations: vec![],
    };
    read.is_valid_read(true).then_some(read).ok_or_else(invalid_data)
}

fn ensure_generation<R: NativeRead>(io: &R, original: Option<u64>) -> Result<()> {
    if original.is_some() && original != io.connection_generation() {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}
