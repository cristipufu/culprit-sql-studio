//! Original cache-entry grant counters describe retained use, not the cause or owner of current memory pressure.
use culprit_core::{
    Error, Metric, Result, SourceOutcome, SourceState, StatementGrantExplanation, StatementGrantRead,
    StatementGrantReport, Unit, format,
    i18n::{Message, MessageValue},
    key, message, t,
    telemetry::{CollectionBudget, DefinitionPayload, SourceObservation, StatementGrantSnapshot},
};
use std::collections::BTreeSet;

mod classify;
pub(crate) use classify::{GrantCase, GrantUse, read_cases};

const STATEMENT_TITLE_CHARACTERS: usize = 160;

pub fn explain(snapshots: &[StatementGrantSnapshot], sources: &[SourceOutcome]) -> Result<StatementGrantReport> {
    explain_read(snapshots, sources, true)
}

/// Decode availability qualifies the derived explanation, never the original native source descriptor.
pub fn explain_read(
    snapshots: &[StatementGrantSnapshot],
    sources: &[SourceOutcome],
    read_complete: bool,
) -> Result<StatementGrantReport> {
    let mut keys = BTreeSet::new();
    let mut entries = Vec::new();
    let mut reads = Vec::new();
    for snapshot in snapshots {
        let cases = read_cases(snapshot, read_complete)?;
        let snapshot_key = snapshot.key()?;
        if !keys.insert(snapshot_key.clone())
            || entries.len().saturating_add(cases.len()) > CollectionBudget::upper_limits().max_snapshot_rows as usize
        {
            return Err(invalid());
        }
        let SourceObservation::Snapshot(description) = &snapshot.description.observation else {
            return Err(invalid());
        };
        reads.push(StatementGrantRead {
            snapshot_key,
            source_scope_id: snapshot.description.captured_range()?.scope_id,
            coverage: description.sources.clone(),
            read_complete,
        });
        entries.extend(cases.into_iter().map(render));
    }
    entries.sort_by(|left, right| {
        (&left.snapshot_key, &left.observation_key).cmp(&(&right.snapshot_key, &right.observation_key))
    });
    reads.sort_by(|left, right| left.snapshot_key.cmp(&right.snapshot_key));
    let mut limitations = limitations(snapshots, sources);
    if !read_complete {
        limitations.push(message!("view.statement_grants.limit.read"));
    }
    Ok(StatementGrantReport {
        sources: sources.to_vec(),
        reads,
        title: message!("view.statement_grants.title"),
        summary: if entries.is_empty() {
            message!("view.statement_grants.summary.empty")
        } else {
            message!(
                "view.statement_grants.summary.entries",
                n = entries.len(),
                count = MessageValue::Digits(entries.len().to_string())
            )
        },
        limitations,
        entries,
    })
}

pub(crate) fn limitations(snapshots: &[StatementGrantSnapshot], sources: &[SourceOutcome]) -> Vec<Message> {
    let mut limitations = vec![
        message!("view.statement_grants.limit.history"),
        message!("view.statement_grants.limit.attribution"),
        message!("view.statement_grants.limit.extrema"),
    ];
    if snapshots.iter().any(|snapshot| !snapshot.description.is_partition_qualified()) {
        limitations.push(message!("view.statement_grants.limit.owner"));
    }
    if sources.is_empty() {
        limitations.push(message!("view.statement_grants.limit.source_unknown"));
    }
    let states = sources
        .iter()
        .map(|source| source.state)
        .chain(
            snapshots
                .iter()
                .filter_map(|snapshot| match &snapshot.description.observation {
                    SourceObservation::Snapshot(description) => Some(description),
                    _ => None,
                })
                .flat_map(|description| &description.sources)
                .map(|source| source.state),
        )
        .collect::<Vec<_>>();
    for (state, message) in [
        (SourceState::Partial, message!("view.statement_grants.limit.source_partial")),
        (SourceState::Unavailable, message!("view.statement_grants.limit.source_unavailable")),
        (SourceState::NotApplicable, message!("view.statement_grants.limit.source_not_applicable")),
    ] {
        if states.contains(&state) {
            limitations.push(message);
        }
    }
    limitations
}

pub(crate) fn render(case: GrantCase<'_>) -> StatementGrantExplanation {
    let row = case.row;
    let mut metrics = Vec::new();
    let mut explanation = vec![match case.usage {
        GrantUse::RecordedImbalance(averages) | GrantUse::RecordedUse(averages) => {
            metrics.extend([
                Metric::said(
                    key!("check.memory_grant.metric.reserved"),
                    averages.granted_kibibytes * 1024.0,
                    Unit::Bytes,
                ),
                Metric::said(key!("check.memory_grant.metric.used"), averages.used_kibibytes * 1024.0, Unit::Bytes),
            ]);
            message!(
                "view.statement_grants.entry.averages",
                granted = MessageValue::Bytes(averages.granted_kibibytes * 1024.0),
                used = MessageValue::Bytes(averages.used_kibibytes * 1024.0),
                count = MessageValue::Digits(row.executions.clone().unwrap_or_default())
            )
        }
        GrantUse::CountersUnavailable => message!("view.statement_grants.entry.unmeasured"),
    }];
    if matches!(case.usage, GrantUse::RecordedImbalance(_)) {
        explanation.push(message!("view.statement_grants.entry.imbalance"));
    }
    if let Some(executions) = &row.executions {
        metrics.push(Metric::said_text(key!("check.memory_grant.metric.runs"), format::digits(executions.clone())));
    }
    for (label, value) in [
        (key!("view.statement_grants.metric.total_grant"), &row.total_grant_kibibytes),
        (key!("view.statement_grants.metric.total_used"), &row.total_used_kibibytes),
        (key!("view.statement_grants.metric.last_grant"), &row.last_grant_kibibytes),
        (key!("view.statement_grants.metric.last_used"), &row.last_used_kibibytes),
        (key!("view.statement_grants.metric.min_grant"), &row.min_grant_kibibytes),
        (key!("view.statement_grants.metric.min_used"), &row.min_used_kibibytes),
        (key!("view.statement_grants.metric.max_grant"), &row.max_grant_kibibytes),
        (key!("view.statement_grants.metric.max_used"), &row.max_used_kibibytes),
    ] {
        if let Some(value) = value {
            metrics.push(Metric::said_text(label, t!("common.bytes.kib", n = format::digits(value.clone()))));
        }
    }
    if row.last_grant_kibibytes.is_some() || row.last_used_kibibytes.is_some() {
        explanation.push(message!("view.statement_grants.entry.last"));
    }
    let title = match &row.statement {
        DefinitionPayload::Available { text, .. } => {
            let mut statement: String = text.chars().take(STATEMENT_TITLE_CHARACTERS).collect();
            if text.chars().count() > STATEMENT_TITLE_CHARACTERS {
                statement.push('…');
            }
            message!("view.statement_grants.entry.named", statement = statement)
        }
        DefinitionPayload::Unavailable { .. } => message!("view.statement_grants.entry.unknown"),
    };
    StatementGrantExplanation {
        snapshot_key: case.snapshot_key,
        observation_key: case.observation_key,
        identity: row.context.identity.clone(),
        title,
        explanation: Message::paragraph(explanation),
        metrics,
        investigation: message!("check.memory_grant.fix"),
        verify: message!("check.memory_grant.verify"),
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.statement_grants.invalid"))
}

#[cfg(test)]
mod tests;
