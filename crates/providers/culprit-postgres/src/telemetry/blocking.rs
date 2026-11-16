//! Selected lock closure reuses Live's native reader; healthy Activity capture never calls it.
use super::*;
use crate::native::{
    activity as native_activity,
    local_context::{self, Context},
};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::collections::BTreeSet;

const SOURCE: &str = "postgres-blocking";
// Setup, two database/member bounds, two actor reads, eight paired closure reads, resources and selected SQL.
const MAX_BLOCKING_QUERIES: u32 = 25;
const MAX_BLOCKING_BYTES: u32 = 1024 * 1024;
const MAX_BLOCKING_SQL_BYTES: usize = 64 * 1024;
const STATEMENT_TIMEOUT_MS: u32 = 1_000;
const TOTAL_TIMEOUT_MS: u32 = 5_000;

pub(super) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_queries = budget.max_queries.min(MAX_BLOCKING_QUERIES);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(MAX_BLOCKING_PARTICIPANTS);
    budget.page_rows = budget.page_rows.min(MAX_BLOCKING_PARTICIPANTS);
    budget.total_bytes = budget.total_bytes.min(MAX_BLOCKING_BYTES);
    budget.page_bytes = budget.page_bytes.min(budget.total_bytes);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(STATEMENT_TIMEOUT_MS);
    budget.total_timeout_ms = budget.total_timeout_ms.min(TOTAL_TIMEOUT_MS);
    budget
}

pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    let CollectionSelection::FocusedBlocking { focus } = &request.selection else { return Err(native::invalid()) };
    if focus.target.engine() != culprit_core::Engine::Postgres {
        return Err(native::invalid());
    }
    pid(&focus.original.key.session.id)?;
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    let reader = Reader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        verified: None,
    };
    Ok(Box::new(FrozenSource::new(reader, &request)?))
}

struct Reader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}

#[async_trait::async_trait]
impl<R: NativeRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let observation = capture(&mut self.io, &self.host, self.port, request).await?;
        self.verified = Some(observation.description.clone());
        Ok(observation)
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(source))
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

async fn capture<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &CollectionRequest,
) -> Result<FrozenObservation> {
    request.validate()?;
    let CollectionSelection::FocusedBlocking { focus } = &request.selection else { return Err(native::invalid()) };
    let pid = pid(&focus.original.key.session.id)?;
    let before = metadata::activity(reader, host, port).await?;
    if before.version < 120_000 {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let owner = Context::observe(reader, &before).await?;
    let initial = match &owner {
        Some(owner) => owner.read_context()?,
        None => local_context::unknown(&before, &before.at)?,
    };
    validate_original(focus, &initial)?;
    let mut frame = actor(reader, &before, pid, &request.budget).await?;
    let current = visible_actor(&frame)?;
    let binding = binding(focus, &initial, current);
    let mut membership = FocusedRequestMembership::NotObserved;
    let mut status = BlockingClosureStatus::NotRead;
    let mut request_end = BlockingRequestState::NotRead;
    if current.is_some_and(|row| same_request(row, focus)) {
        membership = FocusedRequestMembership::Observed;
        if current.is_some_and(|row| row.waiting_on_lock == Some(true)) {
            status = native_activity::observe_blocking(
                reader,
                &mut frame,
                native_activity::BlockingReadOptions {
                    minimum_request_age_ms: 0,
                    version: before.version,
                    database_oid: &before.database_oid,
                    max_snapshot_rows: request.budget.max_snapshot_rows as usize,
                },
            )
            .await?;
        } else {
            for row in &mut frame.records {
                if let Some(facts) = &mut row.facts {
                    facts.blocking_status = ActivityBlockingStatus::Unknown;
                }
            }
        }
        let keys = frame.observations().into_iter().map(|row| row.key).collect::<BTreeSet<_>>();
        let pool = (request.budget.total_bytes as usize / 8).min(MAX_BLOCKING_SQL_BYTES);
        let per_statement = (pool / keys.len().max(1)).min(request.budget.max_sql_bytes as usize);
        native_activity::enrich(reader, &mut frame, &keys, per_statement, false).await?;
        let end = actor(reader, &before, pid, &request.budget).await?;
        request_end = match visible_actor(&end)? {
            None => BlockingRequestState::Missing,
            Some(row) if same_request(row, focus) => BlockingRequestState::SameStatement,
            Some(_) => BlockingRequestState::Changed,
        };
    }
    let context = local_context::complete(reader, host, port, &before, owner.as_ref()).await?;
    let observations = if membership == FocusedRequestMembership::Observed { frame.observations() } else { vec![] };
    let count = observations.len().try_into().map_err(|_| native::invalid())?;
    let mut coverage = StateSourceCoverage::observed(SOURCE.into(), count, false);
    if !before.full_visibility {
        coverage.error_code = Some(ErrorCode::PermissionDenied);
    }
    if status.is_limited() {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    let result = ActivityBlockingFocusResult {
        focus: focus.as_ref().clone(),
        binding,
        membership,
        request_end,
        closure: BlockingClosureDescription {
            status,
            max_lookups: MAX_BLOCKING_LOOKUPS,
            max_depth: MAX_BLOCKING_DEPTH,
            max_participants: request.budget.max_snapshot_rows,
        },
    };
    let description = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedBlocking {
            snapshot: ActivitySnapshotDescription {
                epoch: Some(before.activity_epoch()),
                database_writable: context.database_writable,
                observed_from: context.observed_from,
                observed_until: context.observed_until,
                at: canonical_time(&frame.at)?,
                scope: culprit_core::MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: count,
                observations: count,
                coverage,
            },
            result: Box::new(result),
        },
    };
    if !description.matches_request(request) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let rows =
        observations.iter().map(|row| ActivityFrameRow { observation: row, reference: None }).collect::<Vec<_>>();
    let evidence = ActivityFrameEvidence::new(&description, &rows, true)?;
    let SourceObservation::FocusedBlocking { result, .. } = &description.observation else {
        return Err(native::invalid());
    };
    if !result.accepts_frame(&evidence) {
        return Err(native::invalid());
    }
    Ok(FrozenObservation { description, records: observations.into_iter().map(TelemetryRecord::from).collect() })
}

fn pid(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32 && pid.to_string() == value)
        .ok_or_else(native::invalid)
}
fn validate_original(focus: &ActivityRequestFocus, current: &NativeReadContext) -> Result<()> {
    if focused_request_scope_changed(focus, current) {
        return Err(CollectionStop::SourceChanged.error());
    }
    if matches!(focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) })
        && !focused_request_original_scope_matches(focus, current)
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(())
}
fn same_request(row: &ActivityObservation, focus: &ActivityRequestFocus) -> bool {
    row.key == focus.original.key && row.statement_identity.as_ref() == Some(&focus.statement_identity)
}
fn binding(
    focus: &ActivityRequestFocus,
    current: &NativeReadContext,
    actor: Option<&ActivityObservation>,
) -> FocusedRequestBinding {
    if !focused_request_original_scope_matches(focus, current) {
        return FocusedRequestBinding::UnverifiedOriginalScope;
    }
    match actor {
        None => FocusedRequestBinding::ParticipantMissing,
        Some(row) if same_request(row, focus) => FocusedRequestBinding::Matched,
        Some(_) => FocusedRequestBinding::ParticipantChanged,
    }
}
async fn actor<R: NativeRead>(
    reader: &mut R,
    meta: &Metadata,
    pid: u32,
    budget: &CollectionBudget,
) -> Result<native_activity::ActivityFrame> {
    native_activity::observe_rows(
        reader,
        &native_activity::target_query(meta.version >= 140_000, pid, "'client backend'", ""),
        &meta.database_oid,
        budget,
    )
    .await
}
fn visible_actor(frame: &native_activity::ActivityFrame) -> Result<Option<&ActivityObservation>> {
    frame
        .records
        .first()
        .map(|row| {
            row.facts
                .as_ref()
                .ok_or_else(|| Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")))
        })
        .transpose()
}

#[cfg(test)]
mod tests;
