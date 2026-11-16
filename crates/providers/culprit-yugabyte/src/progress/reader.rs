//! One held local YSQL reader brackets actor, progress and native member/process metadata.
use super::index;
use crate::native::{self, metadata};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use culprit_postgres::wire::{NoticeRead, activity};

pub(super) async fn capture(
    reader: &mut impl NoticeRead,
    host: &str,
    port: u16,
    focus: &ActivityRequestFocus,
    budget: &CollectionBudget,
) -> Result<MaintenanceProgressSnapshot> {
    if !focus.is_valid() || focus.target.engine() != culprit_core::Engine::Yugabyte {
        return Err(native::invalid());
    }
    let (node, pid) = focus.original.key.session.id.split_once('/').ok_or_else(native::invalid)?;
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid > 0 && *pid <= i32::MAX as u32).ok_or_else(native::invalid)?;
    let notices = reader.notices();
    let before = metadata::read(reader, host, port).await?;
    if node != before.node {
        // A local progress view cannot authenticate a remote gv$ participant using this node's process epoch.
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let initial = before.completed_action_context(&before).ok_or_else(native::invalid)?;
    validate_source(focus, &initial)?;
    let first = actor(reader, &before, pid, budget).await?;
    let binding = if !maintenance_progress_original_scope_matches(focus, &initial) {
        MaintenanceProgressBinding::UnverifiedOriginalScope
    } else {
        match &first {
            None => MaintenanceProgressBinding::ParticipantMissing,
            Some(row) if matches_actor(row, focus) => MaintenanceProgressBinding::Matched,
            Some(_) => MaintenanceProgressBinding::ParticipantChanged,
        }
    };
    let mut observations = Vec::new();
    let mut membership = MaintenanceProgressMembership::NotObserved;
    let mut unavailable = None;
    if first.as_ref().is_some_and(|row| matches_actor(row, focus)) {
        match index::read(reader, before.probe.oid, node, &focus.original.key).await {
            Ok(Some(point)) => {
                observations.push(point);
                membership = MaintenanceProgressMembership::Observed;
            }
            Ok(None) => {}
            Err(error) if optional(&error) => {
                unavailable = Some(error.code);
                membership = MaintenanceProgressMembership::Unavailable;
            }
            Err(error) => return Err(error),
        }
    }
    let last = actor(reader, &before, pid, budget).await?;
    if !same_actor(first.as_ref(), last.as_ref()) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let after = metadata::read(reader, host, port).await?;
    let context = before.completed_action_context(&after).ok_or_else(|| CollectionStop::SourceChanged.error())?;
    if reader.notices() != notices {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let coverage = match unavailable {
        Some(code) => StateSourceCoverage::unavailable(index::SOURCE.into(), code, None),
        None => StateSourceCoverage::observed(index::SOURCE.into(), observations.len() as u32, false),
    };
    let snapshot = MaintenanceProgressSnapshot {
        source: index::SOURCE.into(),
        coverage,
        result: MaintenanceProgressResult {
            focus: Some(Box::new(focus.clone())),
            database_writable: context.database_writable,
            binding,
            membership,
        },
        context,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(native::invalid());
    }
    Ok(snapshot)
}

fn validate_source(focus: &ActivityRequestFocus, context: &NativeReadContext) -> Result<()> {
    if focus.target.telemetry_id()? != context.target.telemetry_id()?
        || focus.partition != SourcePartition::Target && !maintenance_progress_original_scope_matches(focus, context)
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}

async fn actor(
    reader: &mut impl NoticeRead,
    metadata: &metadata::Metadata,
    pid: u32,
    budget: &CollectionBudget,
) -> Result<Option<ActivityObservation>> {
    let frame = activity::observe_rows(
        reader,
        &activity::target_query(false, pid, "'client backend'", ""),
        &metadata.probe.oid.to_string(),
        budget,
    )
    .await?;
    let Some(record) = frame.records.first() else { return Ok(None) };
    let mut row = record
        .facts
        .clone()
        .ok_or_else(|| Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")))?;
    row.key.session.id = format!("{}/{}", metadata.node, row.key.session.id);
    row.blocking_status = ActivityBlockingStatus::Unknown;
    if let Some(statement) = &mut row.statement_identity {
        statement.qualifiers.insert("node".into(), metadata.node.clone());
    }
    Ok(Some(row))
}

fn matches_actor(row: &ActivityObservation, focus: &ActivityRequestFocus) -> bool {
    row.key == focus.original.key && row.statement_identity.as_ref() == Some(&focus.statement_identity)
}

fn same_actor(first: Option<&ActivityObservation>, last: Option<&ActivityObservation>) -> bool {
    match (first, last) {
        (None, None) => true,
        (Some(first), Some(last)) => first.key == last.key && first.statement_identity == last.statement_identity,
        _ => false,
    }
}

fn optional(error: &Error) -> bool {
    error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported)
}
