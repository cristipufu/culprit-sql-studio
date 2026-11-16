//! One bounded owner brackets actor, native progress and metadata. Missing rows never prove completion.
use super::{index, read_other_phase};
use crate::{
    native::{self, local_context::Context, metadata},
    read::QueryRead,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    IndexOnly,
    Live,
}

pub(super) struct Acquired {
    pub snapshot: MaintenanceProgressSnapshot,
    pub actor: Option<ActivityObservation>,
    pub sql: Option<String>,
    pub other_phase: Option<(String, Option<f64>, &'static str)>,
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
    if original.is_some_and(|focus| focus.original.key.session.id != pid.to_string()) {
        return Err(native::invalid());
    }
    let before = metadata::activity(reader, host, port).await?;
    let owner = Context::observe(reader, &before).await?;
    let initial = match &owner {
        Some(owner) => owner.read_context()?,
        None => unknown_context(&before, &before.at)?,
    };
    validate_original_source(original, &initial)?;
    let first = actor(reader, &before, pid, budget, mode).await?;
    let binding = binding(original, &initial, first.as_ref());
    let may_read = first.as_ref().is_some_and(|actor| actor.facts.key.request.is_some())
        && original.is_none_or(|expected| actor_matches(first.as_ref(), expected));
    let mut observations = Vec::new();
    let mut membership = MaintenanceProgressMembership::NotObserved;
    let mut unavailable = None;
    let mut other_phase = None;
    if may_read {
        let current = first.as_ref().ok_or_else(native::invalid)?;
        let read = if before.version >= 120_000 {
            index::read(reader, &before.database_oid, &current.facts.key).await
        } else {
            Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))
        };
        match read {
            Ok(Some(row)) => {
                observations.push(row);
                membership = MaintenanceProgressMembership::Observed;
            }
            Ok(None) => {}
            Err(error) if optional(&error) => {
                unavailable = Some(error.code);
                membership = MaintenanceProgressMembership::Unavailable;
            }
            Err(error) => return Err(error),
        }
        if matches!(mode, Mode::Live) && observations.is_empty() {
            other_phase =
                read_other_phase(reader, pid, &before.database_oid, &current.facts.key, before.version).await?;
        }
    }
    let last = actor(reader, &before, pid, budget, mode).await?;
    if !same_actor(first.as_ref(), last.as_ref()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let context = finish_context(reader, host, port, &before, owner.as_ref()).await?;
    let coverage = match unavailable {
        Some(ErrorCode::Unsupported) => StateSourceCoverage::not_applicable(index::SOURCE.into()),
        Some(code) => StateSourceCoverage::unavailable(index::SOURCE.into(), code, None),
        None => StateSourceCoverage::observed(index::SOURCE.into(), observations.len() as u32, false),
    };
    let snapshot = MaintenanceProgressSnapshot {
        source: index::SOURCE.into(),
        result: MaintenanceProgressResult {
            focus: original.cloned().map(Box::new),
            database_writable: context.database_writable,
            binding,
            membership,
        },
        context,
        coverage,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(native::invalid());
    }
    Ok(Acquired {
        snapshot,
        sql: first.as_ref().and_then(|actor| actor.sql.clone()),
        actor: first.map(|actor| actor.facts),
        other_phase,
    })
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
        if qualified && !maintenance_progress_original_scope_matches(focus, current) {
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
        Mode::IndexOnly => String::new(),
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
) -> MaintenanceProgressBinding {
    let Some(focus) = original else { return MaintenanceProgressBinding::Independent };
    if !maintenance_progress_original_scope_matches(focus, context) {
        return MaintenanceProgressBinding::UnverifiedOriginalScope;
    }
    match actor {
        None => MaintenanceProgressBinding::ParticipantMissing,
        Some(_) if actor_matches(actor, focus) => MaintenanceProgressBinding::Matched,
        Some(_) => MaintenanceProgressBinding::ParticipantChanged,
    }
}

async fn finish_context<R: QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    before: &metadata::Metadata,
    owner: Option<&Context>,
) -> Result<NativeReadContext> {
    if let Some(owner) = owner {
        return owner.recheck(reader, host, port).await?.ok_or_else(|| CollectionStop::SourceChanged.error());
    }
    let after = metadata::activity(reader, host, port).await?;
    if !before.same_activity_source(&after) || ObservationBounds::new(&before.at, &after.at).is_none() {
        return Err(CollectionStop::SourceChanged.error());
    }
    // A later permission grant cannot fill the missing initial member or qualify the original read retrospectively.
    let observed_until = match Context::observe(reader, &after).await? {
        Some(owner) => owner.read_context()?.observed_until,
        None => after.at,
    };
    unknown_context(before, &observed_until)
}

fn unknown_context(metadata: &metadata::Metadata, until: &str) -> Result<NativeReadContext> {
    let context = NativeReadContext {
        target: metadata.database.clone().into(),
        partition: SourcePartition::InstanceLocal { member: None, epoch: Some(metadata.activity_epoch()) },
        observed_from: canonical_time(&metadata.at)?,
        observed_until: canonical_time(until)?,
        database_writable: Some(!metadata.standby),
    };
    if !context.is_valid() {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(context)
}

fn optional(error: &Error) -> bool {
    error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
}
