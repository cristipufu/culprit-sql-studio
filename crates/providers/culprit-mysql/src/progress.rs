//! One bounded current-request acquisition feeds interactive progress and retained native evidence.
use crate::{
    connect::{Connector, Flavor, Scope},
    native::{self as source_native, local_context::Context},
    read::{OwnedRead, QueryRead, ReadSession, confirmed_result},
};
use culprit_core::{Engine, QueryProgress, Result, session_number, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

#[cfg(test)]
mod fallback_tests;
mod live;
mod native;
mod session;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub(crate) const SOURCE: &str = culprit_core::source_id::CURRENT_REQUEST_WORK;
const MAX_SOURCE_QUERIES: u32 = 24;
const MAX_LIVE_QUERIES: u32 = 25;
const MAX_STATEMENT_MS: u32 = 1_000;
const MAX_TOTAL_MS: u32 = 5_000;

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    bounded(requested, MAX_SOURCE_QUERIES)
}
fn bounded(requested: &CollectionBudget, queries: u32) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_queries = budget.max_queries.min(queries);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(1);
    budget.page_rows = budget.page_rows.min(1);
    budget.max_sql_bytes = budget.max_sql_bytes.min(MAX_CURRENT_REQUEST_WORK_SQL_BYTES as u32);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(MAX_STATEMENT_MS);
    budget.total_timeout_ms = budget.total_timeout_ms.min(MAX_TOTAL_MS);
    budget
}

pub(crate) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    let CollectionSelection::CurrentRequestWork { focus } = &request.selection else {
        return Err(source_native::invalid());
    };
    validate_selector(focus)?;
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    Ok(Box::new(FrozenSource::new(
        Reader {
            io: ReadSession::new(connector.clone(), request.budget.clone(), Scope::Server),
            connector,
            verified: None,
        },
        &request,
    )?))
}

struct Reader<R = ReadSession> {
    io: R,
    connector: Arc<Connector>,
    verified: Option<CollectionDescription>,
}
#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let CollectionSelection::CurrentRequestWork { focus } = &request.selection else {
            return Err(source_native::invalid());
        };
        let captured = capture_from(
            &mut self.io,
            &self.connector,
            session_number(&focus.original.key.session.id)?,
            Some(focus),
            &request.budget,
            PlanRead::None,
        )
        .await?;
        let snapshot = captured.snapshot;
        let description = description(&snapshot);
        let records = snapshot
            .observations
            .into_iter()
            .map(|row| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: SOURCE.into(),
                    key: row.key()?,
                    fact: OperationalFact::CurrentRequestWork(Box::new(row)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        self.verified = Some(description.clone());
        Ok(FrozenObservation { description, records })
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(source))
    }
    fn query_count(&self) -> u32 {
        self.io.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

fn description(snapshot: &CurrentRequestWorkSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: snapshot.context.target.clone(),
        partition: snapshot.context.partition.clone(),
        source: SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::CurrentRequestWork {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestWork,
                observed_from: snapshot.context.observed_from.clone(),
                observed_until: snapshot.context.observed_until.clone(),
                records: snapshot.coverage.records,
                sources: vec![snapshot.coverage.clone()],
            },
            result: Box::new(snapshot.result.clone()),
        },
    }
}

#[derive(Clone, Copy)]
enum PlanRead {
    None,
    Live { with_plan: bool },
}
impl PlanRead {
    async fn context<R: QueryRead>(self, reader: &mut R, connector: &Connector) -> Result<Context> {
        match self {
            Self::None => Context::capture(reader, connector).await,
            Self::Live { .. } => Context::capture_live(reader, connector).await,
        }
    }
    fn is_live(self) -> bool {
        matches!(self, Self::Live { .. })
    }
}
struct Captured {
    snapshot: CurrentRequestWorkSnapshot,
    point: Option<Box<native::StatementPoint>>,
    plan: live::PlanEvidence,
    flavor: Flavor,
    running: Option<bool>,
    session: Option<session::Capture>,
}

pub(crate) async fn query_progress(
    connector: &Arc<Connector>,
    session: &str,
    with_plan: bool,
) -> Result<QueryProgress> {
    let session_id = session_number(session)?;
    if session_id == 0 || session_id.to_string() != session {
        return Err(source_native::invalid());
    }
    let budget = bounded(&culprit_read::application_budget(std::time::Duration::ZERO), MAX_LIVE_QUERIES);
    let mut reader = ReadSession::new(connector.clone(), budget.clone(), Scope::Server);
    query_progress_from(&mut reader, connector, session_id, &budget, with_plan).await
}
async fn query_progress_from<R: OwnedRead>(
    reader: &mut R,
    connector: &Connector,
    session: u64,
    budget: &CollectionBudget,
    with_plan: bool,
) -> Result<QueryProgress> {
    let result = capture_from(reader, connector, session, None, budget, PlanRead::Live { with_plan })
        .await
        .map(|captured| live::project(&session.to_string(), captured, with_plan));
    confirmed_result(reader, result).await
}

async fn capture_from<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
    session: u64,
    focus: Option<&ActivityRequestFocus>,
    budget: &CollectionBudget,
    plan_read: PlanRead,
) -> Result<Captured> {
    if session == 0 {
        return Err(source_native::invalid());
    }
    if let Some(focus) = focus {
        validate_selector(focus)?;
        if focus.original.key.session.id != session.to_string() {
            return Err(source_native::invalid());
        }
    }
    let before = plan_read.context(reader, connector).await?;
    let initial = before.current_context().ok_or_else(|| CollectionStop::SourceChanged.error())?;
    let initial_binding = bind_original(focus, &initial)?;
    let statement = if before.instrumented {
        match native::statement(
            reader,
            session,
            &before.connection.database_name,
            before.instrumented,
            budget.max_sql_bytes as usize,
        )
        .await
        {
            Ok(statement) => statement,
            Err(error) if plan_read.is_live() && source_native::local_context::unavailable(&error) => {
                native::StatementRead::Unavailable
            }
            Err(error) => return Err(error),
        }
    } else {
        native::StatementRead::Unavailable
    };
    let mut running = match &statement {
        native::StatementRead::Observed(_) => Some(true),
        native::StatementRead::Idle => Some(false),
        _ => None,
    };
    let independent_session = if plan_read.is_live()
        && matches!(statement, native::StatementRead::Unavailable | native::StatementRead::NotObserved)
    {
        Some(session::read(reader, session, &before, budget.max_sql_bytes as usize).await?)
    } else {
        None
    };
    let (mut point, mut membership) = match statement {
        native::StatementRead::Observed(point) => (Some(point), FocusedRequestMembership::Observed),
        native::StatementRead::NotObserved | native::StatementRead::Idle => {
            (None, FocusedRequestMembership::NotObserved)
        }
        native::StatementRead::Unavailable | native::StatementRead::Foreign => {
            (None, FocusedRequestMembership::Unavailable)
        }
    };
    let mut binding = initial_binding;
    if let Some(current) = &point {
        if focus.is_some_and(|focus| {
            current.observation.actor != focus.original.key
                || current.observation.statement_identity != focus.statement_identity
        }) {
            point = None;
            running = None;
            membership = FocusedRequestMembership::NotObserved;
            binding = changed_binding(initial_binding, false);
        } else {
            membership = FocusedRequestMembership::Observed;
        }
    } else if membership == FocusedRequestMembership::NotObserved {
        binding = changed_binding(initial_binding, true);
    }
    let mut plan = live::PlanEvidence::default();
    if let Some(current) = &mut point {
        current.observation.native.stage = read_stage(reader, &before, &current.observation.native).await?;
        if let PlanRead::Live { with_plan } = plan_read
            && initial.has_qualified_local_owner()
        {
            plan = live::plan(
                reader,
                before.connection.flavor,
                &before.connection.version,
                session,
                current,
                with_plan,
                budget.max_plan_bytes as usize,
            )
            .await?;
        }
        let rechecked = native::statement(
            reader,
            session,
            &before.connection.database_name,
            before.instrumented,
            budget.max_sql_bytes as usize,
        )
        .await?;
        match rechecked {
            native::StatementRead::Observed(after) if same_statement(&current.observation, &after.observation) => {}
            native::StatementRead::Unavailable => {
                point = None;
                running = None;
                plan = live::PlanEvidence::default();
                membership = FocusedRequestMembership::Unavailable;
                binding = initial_binding;
            }
            rechecked => {
                point = None;
                running = None;
                plan = live::PlanEvidence::default();
                membership = FocusedRequestMembership::NotObserved;
                binding = changed_binding(initial_binding, matches!(rechecked, native::StatementRead::NotObserved));
            }
        }
    }
    let after = plan_read.context(reader, connector).await?;
    let context = before.checked_context(&after).ok_or_else(|| CollectionStop::SourceChanged.error())?;
    if let Some(session) = &independent_session {
        session.validate(&context)?;
    }
    if !after.instrumented {
        point = None;
        running = None;
        plan = live::PlanEvidence::default();
        membership = FocusedRequestMembership::Unavailable;
        binding = initial_binding;
    } else if let Some(reason) = unavailable_stage(&after)
        && let Some(point) = &mut point
    {
        point.observation.native.stage = MysqlStageObservation::Unavailable { reason };
    }
    let complete = context.has_qualified_local_owner()
        && membership == FocusedRequestMembership::Observed
        && point.as_ref().is_some_and(|point| {
            !matches!(
                point.observation.native.stage,
                MysqlStageObservation::Unavailable { .. } | MysqlStageObservation::Unlinked
            ) && point.observation.scope == ActivityScopeBasis::RequestDatabase
        });
    let mut coverage = StateSourceCoverage::observed(SOURCE.into(), u32::from(point.is_some()), complete);
    if !complete {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let snapshot = CurrentRequestWorkSnapshot {
        source: SOURCE.into(),
        context,
        result: FocusedRequestResult {
            focus: focus.cloned().map(Box::new),
            database_writable: Some(!before.connection.read_only),
            binding,
            membership,
        },
        coverage,
        observations: point.iter().map(|point| point.observation.clone()).collect(),
    };
    if !snapshot.is_valid() {
        return Err(source_native::invalid());
    }
    Ok(Captured { snapshot, point, plan, flavor: before.connection.flavor, running, session: independent_session })
}

fn same_statement(before: &CurrentRequestWorkObservation, after: &CurrentRequestWorkObservation) -> bool {
    before.actor == after.actor
        && before.statement_identity == after.statement_identity
        && before.database == after.database
        && before.scope == after.scope
}
fn bind_original(focus: Option<&ActivityRequestFocus>, context: &NativeReadContext) -> Result<FocusedRequestBinding> {
    let Some(focus) = focus else { return Ok(FocusedRequestBinding::Independent) };
    if focused_request_scope_changed(focus, context) {
        return Err(CollectionStop::SourceChanged.error());
    }
    if focused_request_original_scope_matches(focus, context) {
        return Ok(FocusedRequestBinding::Matched);
    }
    if matches!(&focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(FocusedRequestBinding::UnverifiedOriginalScope)
}
fn changed_binding(original: FocusedRequestBinding, missing: bool) -> FocusedRequestBinding {
    if original != FocusedRequestBinding::Matched {
        original
    } else if missing {
        FocusedRequestBinding::ParticipantMissing
    } else {
        FocusedRequestBinding::ParticipantChanged
    }
}
fn validate_selector(focus: &ActivityRequestFocus) -> Result<()> {
    let session = session_number(&focus.original.key.session.id)?;
    if !focus.is_valid()
        || focus.target.engine() != Engine::Mysql
        || session == 0
        || session.to_string() != focus.original.key.session.id
        || focus
            .original
            .key
            .request
            .as_ref()
            .and_then(|request| {
                culprit_core::engines::mysql_activity_statement_identity(&focus.original.key.session, request)
            })
            .as_ref()
            != Some(&focus.statement_identity)
    {
        return Err(source_native::invalid());
    }
    Ok(())
}
async fn read_stage<R: QueryRead>(
    reader: &mut R,
    context: &Context,
    request: &MysqlCurrentRequestWork,
) -> Result<MysqlStageObservation> {
    if let Some(reason) = unavailable_stage(context) {
        return Ok(MysqlStageObservation::Unavailable { reason });
    }
    match native::stage(reader, request, context.connection.flavor, &context.connection.version).await {
        Ok(stage) => Ok(stage),
        Err(error) if optional(&error) => Ok(MysqlStageObservation::Unavailable {
            reason: if error.code == culprit_core::ErrorCode::PermissionDenied {
                MysqlStageUnavailable::PermissionDenied
            } else {
                MysqlStageUnavailable::Unsupported
            },
        }),
        Err(error) => Err(error),
    }
}
fn unavailable_stage(context: &Context) -> Option<MysqlStageUnavailable> {
    match context.stages_instrumented {
        Some(true) => None,
        Some(false) => Some(MysqlStageUnavailable::InstrumentationDisabled),
        None => Some(MysqlStageUnavailable::NotCollected),
    }
}
fn optional(error: &culprit_core::Error) -> bool {
    error.detail.is_none()
        && matches!(
            error.code,
            culprit_core::ErrorCode::PermissionDenied
                | culprit_core::ErrorCode::Unsupported
                | culprit_core::ErrorCode::NotFound
        )
}
