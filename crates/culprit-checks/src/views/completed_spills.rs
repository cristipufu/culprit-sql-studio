//! Native completed spills name a cache incarnation, not its operator, current request, elapsed cost or cause.
use culprit_core::{
    CompletedSpillEntry, CompletedSpillReport, CompletedSpillState, CompletedSpillStatementBinding, Engine, Metric,
    Result, SourceOutcome, SourceState, format, key, t, telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

mod binding;
#[cfg(test)]
mod tests;
mod validation;

/// The storage owner authenticates these original rows. Pure validation also checks their canonical receipt binding.
pub struct CompletedSpillRecord<'a> {
    pub reference: DiagnosticEvidenceRef,
    pub record: &'a TelemetryRecord,
}

pub struct CompletedSpillEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub rows: &'a [CompletedSpillRecord<'a>],
    pub sources: &'a [SourceOutcome],
    pub read_complete: bool,
}

pub fn explain(
    runtime: CompletedSpillEvidence<'_>,
    definitions: Option<CompletedSpillEvidence<'_>>,
) -> Result<CompletedSpillReport> {
    let context = validation::capture(&runtime, SnapshotDataset::CachedRuntime)?;
    let definition_context = definitions
        .as_ref()
        .map(|input| {
            validation::capture(
                input,
                input
                    .source
                    .observation
                    .snapshot()
                    .filter(|snapshot| {
                        matches!(snapshot.dataset, SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans)
                    })
                    .ok_or_else(invalid)?
                    .dataset,
            )
        })
        .transpose()?;
    let index = binding::definitions(definitions.as_ref())?;
    let mut entries = Vec::with_capacity(runtime.rows.len());
    for row in runtime.rows {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlCachedRuntime(sample),
            ..
        }) = row.record
        else {
            return Err(invalid());
        };
        let pages = sample.counters.get(&CachedRuntimeMetric::SpilledPages);
        let state = match pages {
            None => CompletedSpillState::Unavailable,
            Some(pages)
                if [&pages.total, &pages.last, &pages.minimum, &pages.maximum]
                    .into_iter()
                    .any(|value| value.parse::<u64>().is_ok_and(|value| value > 0)) =>
            {
                CompletedSpillState::RecordedSpills
            }
            Some(_) => CompletedSpillState::NoRecordedSpills,
        };
        let bound = binding::bind(&context, definition_context.as_ref(), &index, sample)?;
        let mut metrics = Vec::new();
        if let Some(pages) = pages {
            for (label, value) in [
                (key!("view.completed_spills.metric.total"), &pages.total),
                (key!("view.completed_spills.metric.last"), &pages.last),
                (key!("view.completed_spills.metric.minimum"), &pages.minimum),
                (key!("view.completed_spills.metric.maximum"), &pages.maximum),
            ] {
                metrics.push(Metric::said_text(label, format::digits(value.clone())));
            }
        }
        if let Some(count) = &sample.executions {
            metrics.push(Metric::said_text(
                key!("view.completed_spills.metric.executions"),
                format::digits(count.clone()),
            ));
        }
        if let Some(at) = &sample.last_execution_without_offset {
            metrics.push(Metric::said_text(key!("view.completed_spills.metric.last_start"), at.clone()));
        }
        let (title, explanation) = match state {
            CompletedSpillState::RecordedSpills => {
                (t!("view.completed_spills.entry.recorded"), t!("view.completed_spills.entry.recorded_detail"))
            }
            CompletedSpillState::NoRecordedSpills => {
                (t!("view.completed_spills.entry.zero"), t!("view.completed_spills.entry.zero_detail"))
            }
            CompletedSpillState::Unavailable => {
                (t!("view.completed_spills.entry.unavailable"), t!("view.completed_spills.entry.unavailable_detail"))
            }
        };
        let origin = match sample.context.origin {
            StatementOrigin::CulpritMonitoring | StatementOrigin::Culprit => t!("view.completed_spills.origin.culprit"),
            StatementOrigin::Application => t!("view.completed_spills.origin.application"),
            StatementOrigin::EngineInternal => t!("view.completed_spills.origin.internal"),
            StatementOrigin::Unknown => t!("view.completed_spills.origin.unknown"),
        };
        entries.push(CompletedSpillEntry {
            observation: row.reference.clone(),
            identity: sample.context.identity.clone(),
            database_id: sample.context.database_id,
            origin: sample.context.origin,
            reset: sample.reset.clone(),
            monitoring_template_id: sample.context.monitoring_template_id.clone(),
            state,
            statement_binding: bound.state,
            definition: bound.reference,
            statement_preview: bound.preview,
            statement_preview_truncated: bound.truncated,
            statement_unavailable: bound.unavailable,
            title,
            explanation: format!("{explanation} {origin} {}", binding::explanation(bound.state)),
            metrics,
            investigation: t!("view.completed_spills.investigate"),
            verify: t!("view.completed_spills.verify"),
        });
    }
    entries.sort_by(|left, right| left.observation.record_id.cmp(&right.observation.record_id));
    let mut limitations = vec![
        t!("view.completed_spills.limit.population"),
        t!("view.completed_spills.limit.cause"),
        t!("view.completed_spills.limit.clock"),
        t!("view.completed_spills.limit.competing"),
    ];
    if context.database_writable.is_none()
        || definition_context.as_ref().is_some_and(|context| context.database_writable.is_none())
    {
        limitations.push(t!("view.completed_spills.limit.role"));
    }
    if !runtime.read_complete
        || runtime.sources.iter().any(|source| !matches!(source.state, SourceState::Collected | SourceState::Empty))
        || definitions.as_ref().is_some_and(|input| {
            !input.read_complete
                || input
                    .sources
                    .iter()
                    .any(|source| !matches!(source.state, SourceState::Collected | SourceState::Empty))
        })
    {
        limitations.push(t!("view.completed_spills.limit.partial"));
    }
    Ok(CompletedSpillReport {
        context,
        sources: runtime.sources.to_vec(),
        read_complete: runtime.read_complete,
        native_rows: runtime.source.observation.snapshot().ok_or_else(invalid)?.records,
        observed_rows: runtime.rows.len().try_into().map_err(|_| invalid())?,
        definition_context,
        definition_sources: definitions.as_ref().map(|input| input.sources.to_vec()).unwrap_or_default(),
        definition_read_complete: definitions.as_ref().map(|input| input.read_complete),
        title: t!("view.completed_spills.title"),
        summary: if entries.is_empty() {
            t!("view.completed_spills.summary.missing")
        } else {
            t!("view.completed_spills.summary.observed", count = format::digits(entries.len().to_string()))
        },
        limitations,
        entries,
    })
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
