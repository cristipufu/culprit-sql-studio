//! One pure interpretation of original CDC snapshots. No event deduplication, cross-capture delta or health zero.
use culprit_core::{
    CdcEntry, CdcEntryKind, CdcField, CdcReport, ErrorCode, Result, SourceState, format, t, telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

pub struct CdcEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<CdcReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(CdcEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: CdcEvidence<'_>) -> Result<CdcReport> {
    let QualifiedEvidence { context, sources, points } = validate(&evidence)?;
    let status = points.iter().find_map(|(_, point)| match &point.data {
        MssqlCdcData::Status(status) => Some(status),
        _ => None,
    });
    let enabled = status.and_then(|status| status.enabled);
    let errors = points.iter().filter(|(_, point)| matches!(point.data, MssqlCdcData::Error(_))).count();
    let native_complete = sources
        .iter()
        .all(|source| matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::NotApplicable));
    let source_state = if !evidence.read_complete || !native_complete {
        if points.is_empty() { SourceState::Unavailable } else { SourceState::Partial }
    } else {
        SourceState::Collected
    };
    let summary = if errors > 0 {
        t!("view.cdc.summary.errors", count = format::digits(errors.to_string()))
    } else if enabled == Some(false) {
        t!("view.cdc.summary.disabled")
    } else if source_state == SourceState::Collected {
        t!("view.cdc.summary.recorded")
    } else {
        t!("view.cdc.summary.unavailable")
    };
    let mut limitations = vec![t!("view.cdc.limit.snapshot"), t!("view.cdc.limit.reset"), t!("view.cdc.limit.clocks")];
    if source_state == SourceState::Partial || !evidence.read_complete {
        limitations.push(t!("view.cdc.limit.partial"));
    }
    if context.database_writable == Some(false) {
        limitations.push(t!("view.cdc.limit.read_only"));
    }
    for source in &sources {
        if let Some(stop) = source.stopped {
            limitations.push(stop.error().message);
        } else if let Some(code) = source.error_code {
            limitations.push(match code {
                ErrorCode::PermissionDenied => match status {
                    Some(status) => t!("view.cdc.limit.permission", permission = status.permission.native_name()),
                    None => t!("view.cdc.limit.permission_unknown"),
                },
                ErrorCode::Unsupported => t!("view.cdc.limit.unsupported"),
                _ => t!("view.cdc.limit.failed"),
            });
        }
    }
    let mut unique = BTreeSet::new();
    limitations.retain(|value| unique.insert(value.clone()));
    let entries = points
        .iter()
        .filter(|(_, point)| matches!(point.data, MssqlCdcData::Error(_)))
        .chain(points.iter().filter(|(_, point)| matches!(point.data, MssqlCdcData::ScanSession(_))))
        .map(|(row, point)| entry(row, point))
        .collect();
    Ok(CdcReport {
        context,
        sources,
        source_state,
        read_complete: evidence.read_complete,
        enabled,
        title: t!("view.cdc.title"),
        summary,
        limitations,
        entries,
    })
}
struct QualifiedEvidence<'a> {
    context: NativeReadContext,
    sources: Vec<StateSourceCoverage>,
    points: Vec<(&'a OperationalObservation, &'a MssqlCdcObservation)>,
}
fn validate<'a>(evidence: &CdcEvidence<'a>) -> Result<QualifiedEvidence<'a>> {
    let header = evidence.source.observation.snapshot().ok_or_else(invalid)?;
    let fallback = NativeReadContext::from_snapshot(evidence.source)?;
    if !cdc_header_is_valid(header)
        || evidence.source.target.engine() != culprit_core::Engine::Mssql
        || !fallback.has_qualified_local_owner()
        || evidence.source.continuity != SourceContinuity::Unverified
    {
        return Err(invalid());
    }
    let mut sources =
        header.sources.iter().map(|source| (source.source.as_str(), (source, 0usize))).collect::<BTreeMap<_, _>>();
    let mut points = Vec::new();
    let mut keys = BTreeSet::new();
    let mut original: Option<&MssqlCdcObservation> = None;
    for row in evidence.observations {
        let OperationalFact::MssqlCdc(point) = &row.fact else { return Err(invalid()) };
        let Some((coverage, count)) = sources.get_mut(row.source.as_str()) else { return Err(invalid()) };
        if !row.is_valid()
            || !point.fits_source(evidence.source)
            || !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
            || point.position > coverage.records
            || !keys.insert((row.source.as_str(), row.key.as_str()))
            || original
                .is_some_and(|original| original.database_id != point.database_id || original.context != point.context)
        {
            return Err(invalid());
        }
        original = Some(point);
        *count += 1;
        points.push((*row, point.as_ref()));
    }
    if sources.values().any(|(source, count)| {
        *count > source.records as usize || evidence.read_complete && *count != source.records as usize
    }) {
        return Err(invalid());
    }
    let context = original.map(|point| point.context.clone()).unwrap_or(fallback);
    Ok(QualifiedEvidence { context, sources: header.sources.clone(), points })
}
fn entry(row: &OperationalObservation, point: &MssqlCdcObservation) -> CdcEntry {
    let (kind, title, explanation, fields, investigation, verify) = match &point.data {
        MssqlCdcData::Error(error) => (
            CdcEntryKind::Error,
            t!("view.cdc.entry.error", number = format::digits(error.error_number.to_string())),
            t!("view.cdc.entry.error_context"),
            vec![
                field("session_id", t!("view.cdc.field.scan_id"), Some(error.session_id.to_string())),
                field("phase_number", t!("view.cdc.field.phase"), error.phase_number.map(|value| value.to_string())),
                field("error_severity", t!("view.cdc.field.severity"), Some(error.severity.to_string())),
                field("error_state", t!("view.cdc.field.error_state"), Some(error.state.to_string())),
                field("entry_time", t!("view.cdc.field.entered"), error.entered_without_offset.clone()),
            ],
            t!("view.cdc.error.investigate"),
            t!("view.cdc.error.verify"),
        ),
        MssqlCdcData::ScanSession(scan) => {
            let aggregate = scan.session_id == 0;
            (
                if aggregate { CdcEntryKind::Aggregate } else { CdcEntryKind::ScanSession },
                if aggregate {
                    t!("view.cdc.entry.aggregate")
                } else {
                    t!("view.cdc.entry.scan", id = format::digits(scan.session_id.to_string()))
                },
                if aggregate { t!("view.cdc.entry.aggregate_context") } else { t!("view.cdc.entry.scan_context") },
                vec![
                    field("scan_phase", t!("view.cdc.field.phase"), scan.native_phase.clone()),
                    field("start_time", t!("view.cdc.field.started"), scan.started_without_offset.clone()),
                    field("end_time", t!("view.cdc.field.ended"), scan.ended_without_offset.clone()),
                    field(
                        "tran_count",
                        t!("view.cdc.field.transactions"),
                        scan.transactions.clone().map(format::digits),
                    ),
                    field(
                        "log_record_count",
                        t!("view.cdc.field.records"),
                        scan.log_records.clone().map(format::digits),
                    ),
                    field("error_count", t!("view.cdc.field.errors"), scan.error_count.clone().map(format::digits)),
                    field("latency", t!("view.cdc.field.latency"), scan.latency_seconds.clone().map(format::digits)),
                    field(
                        "last_commit_cdc_time",
                        t!("view.cdc.field.last_commit"),
                        scan.last_cdc_commit_without_offset.clone(),
                    ),
                ],
                t!("view.cdc.scan.investigate"),
                t!("view.cdc.scan.verify"),
            )
        }
        MssqlCdcData::Status(_) => unreachable!("only native error and scan variants render entries"),
    };
    CdcEntry {
        source: row.source.clone(),
        observation_key: row.key.clone(),
        kind,
        observed_at: point.observed_at.clone(),
        title,
        explanation,
        fields,
        investigation,
        verify,
    }
}
fn field(key: &str, label: String, value: Option<String>) -> CdcField {
    CdcField { key: key.into(), label, value }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
