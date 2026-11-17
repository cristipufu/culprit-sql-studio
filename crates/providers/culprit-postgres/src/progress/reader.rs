//! One bounded owner brackets actor, native progress and metadata. Missing rows never prove completion.
use super::{copy, maintenance};
use crate::{
    native::{
        self,
        local_context::{self, Context},
        metadata,
    },
    read::QueryRead,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Retained,
    CurrentWork,
    Live,
}

pub(super) struct Acquired {
    pub progress: CapturedProgress,
    pub actor: Option<ActivityObservation>,
    pub sql: Option<String>,
}

pub(super) enum CapturedProgress {
    Maintenance(MaintenanceProgressSnapshot),
    CurrentWork(CurrentRequestWorkSnapshot),
}
impl CapturedProgress {
    pub fn context(&self) -> &NativeReadContext {
        match self {
            Self::Maintenance(snapshot) => &snapshot.context,
            Self::CurrentWork(snapshot) => &snapshot.context,
        }
    }
}

enum NativeProgress {
    Maintenance(maintenance::Read),
    CurrentWork(copy::Read),
}
impl NativeProgress {
    fn empty(mode: Mode) -> Self {
        match mode {
            Mode::CurrentWork => Self::CurrentWork(copy::Read { observation: None, unavailable: None }),
            _ => Self::Maintenance(maintenance::Read {
                source: maintenance::SOURCE,
                observation: None,
                unavailable: None,
            }),
        }
    }
    fn finish(
        self,
        context: NativeReadContext,
        original: Option<&ActivityRequestFocus>,
        binding: FocusedRequestBinding,
    ) -> Result<CapturedProgress> {
        let (source, unavailable, records) = match &self {
            Self::Maintenance(read) => (read.source, read.unavailable, u32::from(read.observation.is_some())),
            Self::CurrentWork(read) => (copy::SOURCE, read.unavailable, u32::from(read.observation.is_some())),
        };
        let membership = if records != 0 {
            FocusedRequestMembership::Observed
        } else if unavailable.is_some() {
            FocusedRequestMembership::Unavailable
        } else {
            FocusedRequestMembership::NotObserved
        };
        let coverage = match unavailable {
            Some(code) => StateSourceCoverage::unavailable(source.into(), code, None),
            None => StateSourceCoverage::observed(
                source.into(),
                records,
                matches!(&self, Self::CurrentWork(_)) && records != 0 && context.has_qualified_local_owner(),
            ),
        };
        let result = FocusedRequestResult {
            focus: original.cloned().map(Box::new),
            database_writable: context.database_writable,
            binding,
            membership,
        };
        match self {
            Self::Maintenance(read) => {
                let snapshot = MaintenanceProgressSnapshot {
                    source: source.into(),
                    context,
                    coverage,
                    result,
                    observations: read.observation.into_iter().collect(),
                };
                if !snapshot.is_valid() {
                    return Err(native::invalid());
                }
                Ok(CapturedProgress::Maintenance(snapshot))
            }
            Self::CurrentWork(read) => {
                let snapshot = CurrentRequestWorkSnapshot {
                    profile: CurrentRequestWorkProfile::PostgresCopy,
                    source: source.into(),
                    context,
                    coverage,
                    result,
                    observations: read.observation.into_iter().collect(),
                };
                if !snapshot.is_valid() {
                    return Err(native::invalid());
                }
                Ok(CapturedProgress::CurrentWork(snapshot))
            }
        }
    }
}

struct Actor {
    facts: ActivityObservation,
    sql: Option<String>,
}

pub(super) async fn capture<R: QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    pid: u32,
    original: Option<&ActivityRequestFocus>,
    budget: &CollectionBudget,
    mode: Mode,
) -> Result<Acquired> {
    validate_original(original)?;
    if original.is_some_and(|focus| {
        focus.original.key.session.id != pid.to_string()
            || matches!(mode, Mode::CurrentWork) && !focus.is_current_request_work_focus()
    }) {
        return Err(native::invalid());
    }
    let before = metadata::activity(reader, host, port).await?;
    // backend_type is the native evidence separating this manual/client profile from autovacuum workers.
    if before.version < 100_000 {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let owner = Context::observe(reader, &before).await?;
    let initial = match &owner {
        Some(owner) => owner.read_context()?,
        None => local_context::unknown(&before, &before.at)?,
    };
    validate_original_source(original, &initial)?;
    let first = actor(reader, &before, pid, budget, mode).await?;
    let binding = binding(original, &initial, first.as_ref());
    let may_read = first.as_ref().is_some_and(|actor| actor.facts.key.request.is_some())
        && original.is_none_or(|expected| actor_matches(first.as_ref(), expected));
    let progress = match first.as_ref().filter(|_| may_read) {
        Some(current) => read_progress(reader, &before, current, mode).await?,
        None => NativeProgress::empty(mode),
    };
    let last = actor(reader, &before, pid, budget, mode).await?;
    if !same_actor(first.as_ref(), last.as_ref()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let context = local_context::complete(reader, host, port, &before, owner.as_ref()).await?;
    Ok(Acquired {
        progress: progress.finish(context, original, binding)?,
        sql: first.as_ref().and_then(|actor| actor.sql.clone()),
        actor: first.map(|actor| actor.facts),
    })
}

async fn read_progress<R: QueryRead>(
    reader: &mut R,
    before: &metadata::Metadata,
    current: &Actor,
    mode: Mode,
) -> Result<NativeProgress> {
    if matches!(mode, Mode::CurrentWork) {
        return Ok(NativeProgress::CurrentWork(
            copy::read(
                reader,
                &before.database_oid,
                &before.database.database,
                &current.facts,
                before.version,
                current.sql.as_deref(),
            )
            .await?,
        ));
    }
    let read = maintenance::read(reader, &before.database_oid, &current.facts.key, before.version).await?;
    if matches!(mode, Mode::Live) && read.observation.is_none() && before.version >= copy::MINIMUM_VERSION {
        let copy = copy::read(
            reader,
            &before.database_oid,
            &before.database.database,
            &current.facts,
            before.version,
            current.sql.as_deref(),
        )
        .await?;
        if copy.observation.is_some() || copy.unavailable.is_some() {
            return Ok(NativeProgress::CurrentWork(copy));
        }
    }
    Ok(NativeProgress::Maintenance(read))
}

fn validate_original(original: Option<&ActivityRequestFocus>) -> Result<()> {
    if original.is_some_and(|focus| !focus.is_valid() || focus.target.engine() != culprit_core::Engine::Postgres) {
        return Err(native::invalid());
    }
    Ok(())
}

fn validate_original_source(original: Option<&ActivityRequestFocus>, current: &NativeReadContext) -> Result<()> {
    if let Some(focus) = original {
        if focus.target.telemetry_id()? != current.target.telemetry_id()?
            || matches!((focus.database_writable, current.database_writable), (Some(before), Some(after)) if before != after)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let qualified = matches!(&focus.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) });
        if qualified && !current.has_qualified_local_owner() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        if qualified && !focused_request_original_scope_matches(focus, current) {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    Ok(())
}

async fn actor<R: QueryRead>(
    reader: &mut R,
    metadata: &metadata::Metadata,
    pid: u32,
    budget: &CollectionBudget,
    mode: Mode,
) -> Result<Option<Actor>> {
    let extra = match mode {
        Mode::Retained | Mode::CurrentWork => String::new(),
        Mode::Live => format!(
            ",CASE WHEN pg_catalog.octet_length(a.query)<={} THEN a.query END AS current_sql",
            budget.max_sql_bytes
        ),
    };
    let frame = native::activity::observe_rows(
        reader,
        &native::activity::target_query(metadata.version >= 140_000, pid, "'client backend'", &extra),
        &metadata.database_oid,
        budget,
    )
    .await?;
    let Some(record) = frame.records.first() else { return Ok(None) };
    let facts = record
        .facts
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")))?
        .clone();
    let sql = matches!(mode, Mode::Live)
        .then(|| record.native_text("current_sql"))
        .flatten()
        .filter(|sql| !sql.is_empty() && sql != "<insufficient privilege>");
    Ok(Some(Actor { facts, sql }))
}

fn actor_matches(actor: Option<&Actor>, focus: &ActivityRequestFocus) -> bool {
    actor.is_some_and(|actor| {
        actor.facts.key == focus.original.key
            && actor.facts.statement_identity.as_ref() == Some(&focus.statement_identity)
    })
}

fn same_actor(before: Option<&Actor>, after: Option<&Actor>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => {
            before.facts.key == after.facts.key && before.facts.statement_identity == after.facts.statement_identity
        }
        _ => false,
    }
}

fn binding(
    original: Option<&ActivityRequestFocus>,
    context: &NativeReadContext,
    actor: Option<&Actor>,
) -> FocusedRequestBinding {
    let Some(focus) = original else { return FocusedRequestBinding::Independent };
    if !focused_request_original_scope_matches(focus, context) {
        return FocusedRequestBinding::UnverifiedOriginalScope;
    }
    match actor {
        None => FocusedRequestBinding::ParticipantMissing,
        Some(_) if actor_matches(actor, focus) => FocusedRequestBinding::Matched,
        Some(_) => FocusedRequestBinding::ParticipantChanged,
    }
}

pub(super) fn optional(error: &Error) -> bool {
    error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
}
