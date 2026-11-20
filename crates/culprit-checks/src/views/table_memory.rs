//! One pure interpretation of the selected native table's current allocations, without pressure attribution.
use culprit_core::{
    Metric, Result, SourceOutcome, SourceState, TableMemoryOrigin, TableMemoryReport, format, t, telemetry::*,
};

pub struct TableMemoryEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<TableMemoryReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(TableMemoryEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: TableMemoryEvidence<'_>) -> Result<TableMemoryReport> {
    let context = table_memory_context(evidence.source)?;
    let SourceObservation::TableMemory { snapshot, target, metadata, .. } = &evidence.source.observation else {
        return Err(invalid());
    };
    let [coverage] = snapshot.sources.as_slice() else { return Err(invalid()) };
    let point = match evidence.observations {
        [] => None,
        [row] => {
            let OperationalFact::MssqlTableMemory(point) = &row.fact else { return Err(invalid()) };
            if !row.is_valid()
                || !point.fits_source(evidence.source)
                || !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
            {
                return Err(invalid());
            }
            Some(point.as_ref())
        }
        _ => return Err(invalid()),
    };
    let observed = u32::from(point.is_some());
    if observed > coverage.records || evidence.read_complete && observed != coverage.records {
        return Err(invalid());
    }
    let missing = !evidence.read_complete || observed != coverage.records;
    let state = if missing {
        if observed == 0 { SourceState::Unavailable } else { SourceState::Partial }
    } else {
        coverage.state
    };
    let reason = if missing {
        Some(t!("view.table_memory.partial"))
    } else if let Some(stop) = coverage.stopped {
        Some(stop.error().message)
    } else {
        coverage.error_code.map(|code| match code {
            culprit_core::ErrorCode::PermissionDenied => t!("view.table_memory.permission"),
            culprit_core::ErrorCode::Unsupported => t!("view.table_memory.unsupported"),
            _ => t!("view.table_memory.unavailable"),
        })
    };
    let summary = if point.is_some() {
        t!("view.table_memory.recorded")
    } else if state == SourceState::NotApplicable {
        t!("view.table_memory.not_applicable")
    } else if state == SourceState::Empty {
        t!("view.table_memory.missing_row")
    } else {
        t!("view.table_memory.unavailable")
    };
    let metrics = point
        .map(|point| {
            vec![
                quantity(culprit_core::key!("view.table_memory.allocated_table"), &point.allocated_table_kibibytes),
                quantity(culprit_core::key!("view.table_memory.used_table"), &point.used_table_kibibytes),
                quantity(culprit_core::key!("view.table_memory.allocated_indexes"), &point.allocated_indexes_kibibytes),
                quantity(culprit_core::key!("view.table_memory.used_indexes"), &point.used_indexes_kibibytes),
            ]
        })
        .unwrap_or_default();
    let mut limitations = vec![
        t!("view.table_memory.limit.point"),
        t!("view.table_memory.limit.historical"),
        t!("view.table_memory.limit.versions"),
        t!("view.table_memory.limit.population"),
        t!("view.table_memory.limit.quantities"),
    ];
    if state == SourceState::Partial || missing {
        limitations.push(t!("view.table_memory.partial"));
    }
    Ok(TableMemoryReport {
        original: Some(TableMemoryOrigin {
            context,
            target: target.as_ref().clone(),
            metadata: metadata.as_ref().clone(),
        }),
        source: SourceOutcome { source: coverage.source.clone(), state, reason, reason_message: None },
        read_complete: evidence.read_complete,
        native_rows: Some(coverage.records),
        observed_rows: observed,
        title: t!("view.table_memory.title"),
        summary,
        metrics,
        limitations,
    })
}
pub fn explain_recorded(recorded: &RecordedOperationalRead) -> Result<TableMemoryReport> {
    match recorded {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> TableMemoryReport {
    TableMemoryReport {
        original: None,
        source: SourceOutcome {
            source: MSSQL_TABLE_MEMORY_SOURCE.into(),
            state: SourceState::Unavailable,
            reason: Some(reason.render()),
            reason_message: Some(reason.clone()),
        },
        read_complete: false,
        native_rows: None,
        observed_rows: 0,
        title: t!("view.table_memory.title"),
        summary: reason.render(),
        metrics: Vec::new(),
        limitations: vec![
            t!("view.table_memory.limit.point"),
            t!("view.table_memory.limit.historical"),
            t!("view.table_memory.limit.quantities"),
        ],
    }
}
fn quantity(key: &'static str, value: &Option<String>) -> Metric {
    Metric::said_text(
        key,
        value.as_ref().map(|value| format::digits(value.clone())).unwrap_or_else(|| t!("view.table_memory.unknown")),
    )
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
