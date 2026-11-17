//! Exact native fragment facts and explicitly derived plan statistics share one explanation policy.
use super::query_store_definitions::{
    QueryStoreDefinitionEvidence, QueryStoreDefinitions, QueryStoreStatementScope, runtime_query_identity,
};
use culprit_core::{
    Error, Metric, Result, SourceOutcome, SourceState, Workload, format, key,
    page_server_read_report::*,
    t,
    telemetry::{
        CapturedReadRef, CapturedRecordReference, CollectionDescription, CollectionTarget, PageServerReadAvailability,
        PageServerReadStatistics, PageServerReadSummary, ReferenceCoverage, RuntimeMeasurement, SourcePartition,
        TelemetryRecord,
    },
};
use std::collections::BTreeSet;
mod definitions;
mod snapshot;
pub use snapshot::explain_snapshot;

const MAX_RETAINED_ROWS: usize = 100_000;
const MAX_DETAIL_ENTRIES: usize = 256;

pub struct RuntimeEvidence<'a> {
    pub reference: CapturedRecordReference,
    pub runtime: &'a RuntimeMeasurement,
}

pub struct PageServerReadEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub read: &'a CapturedReadRef,
    pub reference_coverage: &'a ReferenceCoverage,
    pub rows: &'a [RuntimeEvidence<'a>],
    pub selected_rows: u32,
    pub read_complete: bool,
    pub definitions: Option<QueryStoreDefinitionEvidence<'a>>,
}

pub fn explain_archive(input: PageServerReadEvidence<'_>) -> Result<PageServerReadReport> {
    let PageServerReadEvidence { source, read, reference_coverage, rows, selected_rows, read_complete, definitions } =
        input;
    source.captured_range().map_err(|_| invalid())?;
    let original = read.bind(source)?;
    if !matches!(source.target, CollectionTarget::Database(_))
        || source.partition != SourcePartition::Target
        || source.query_history().is_none_or(|history| !history.is_valid())
        || read.streams.len() != 1
        || read.captured_rows(culprit_core::telemetry::TelemetryStream::QueryRuntime) != Some(selected_rows)
        || !reference_coverage.is_valid()
        || rows.len() > MAX_RETAINED_ROWS
        || rows.len() > selected_rows as usize
        || read_complete && rows.len() != selected_rows as usize
    {
        return Err(invalid());
    }
    let availability = source
        .query_history()
        .and_then(|history| history.native_properties.get(PageServerReadAvailability::PROPERTY))
        .map(|value| PageServerReadAvailability::from_native_property(value).ok_or_else(invalid))
        .transpose()?;
    let definitions = definitions
        .map(|input| QueryStoreDefinitions::new(QueryStoreStatementScope::History(source), input))
        .transpose()?;
    let mut physical_rows = BTreeSet::new();
    let mut entries = Vec::with_capacity(rows.len().min(MAX_DETAIL_ENTRIES));
    for row in rows {
        original.validate_canonical_record(&row.reference, &TelemetryRecord::QueryRuntime(row.runtime.clone()))?;
        if !physical_rows.insert(row.reference.row_ordinal())
            || row.reference.is_informational() && reference_coverage.complete
        {
            return Err(invalid());
        }
        let identity =
            definitions.as_ref().map(|_| runtime_query_identity(source, &row.runtime.key)).transpose()?.flatten();
        if entries.len() == MAX_DETAIL_ENTRIES {
            continue;
        }
        let statement = definitions
            .as_ref()
            .map(|definitions| self::definitions::statement(definitions.resolve(identity.as_ref())));
        let mut explanation = match row.runtime.page_server_reads.as_deref() {
            Some(statistics) => explain_statistics(statistics, None)?,
            None => explain_statistics(&PageServerReadStatistics::default(), None)?,
        };
        if row.reference.is_informational() {
            explanation.limitations.push(t!("view.page_server_reads.limit.identity"));
        }
        entries.push(PageServerReadEntry {
            identity: PageServerReadEntryIdentity::RuntimeFragment {
                reference: Box::new(row.reference.clone()),
                native: Box::new(row.runtime.key.clone()),
            },
            executions: row.runtime.executions.clone(),
            explanation,
            statement,
        });
    }
    let state = if !read_complete || !reference_coverage.complete {
        SourceState::Partial
    } else if rows.is_empty() {
        SourceState::Empty
    } else {
        SourceState::Collected
    };
    let mut limitations = common_limitations();
    if !reference_coverage.complete {
        limitations.push(t!("view.page_server_reads.limit.references"));
    }
    if let Some(qualification) = availability_qualification(availability) {
        limitations.push(qualification);
    }
    let definition_read = definitions.as_ref().map(|definitions| Box::new(definitions.report()));
    if let Some(read) = &definition_read {
        self::definitions::append_limitations(read, &mut limitations);
    }
    let presentation_limited = rows.len() > MAX_DETAIL_ENTRIES;
    if presentation_limited {
        limitations.push(t!(
            "view.page_server_reads.limit.presentation",
            displayed = format::digits(entries.len().to_string())
        ));
    }
    if !read_complete {
        limitations.push(t!("view.page_server_reads.limit.read"));
    }
    Ok(PageServerReadReport {
        source: source.source.clone(),
        availability,
        read_complete,
        selected_rows: Some(selected_rows),
        observed_rows: rows.len() as u32,
        presentation_limited,
        source_state: Some(state),
        source_reason: None,
        title: t!("view.page_server_reads.title"),
        summary: t!(
            "view.page_server_reads.summary.archive",
            observed = format::digits(rows.len().to_string()),
            selected = format::digits(selected_rows.to_string())
        ),
        limitations,
        entries,
        runtime_read: Some(Box::new(read.clone())),
        reference_coverage: Some(Box::new(reference_coverage.clone())),
        definition_read,
    })
}

pub fn explain_workload(workload: &Workload, source: Option<&SourceOutcome>) -> Result<PageServerReadReport> {
    let mut entries = Vec::new();
    let mut observed_rows = 0;
    let mut availability = None;
    let mut mixed_availability = false;
    for query in &workload.queries {
        for plan in &query.plans {
            let Some(summary) = plan.page_server_reads.as_deref() else { continue };
            if !summary.is_valid()
                || plan.native_identity.as_ref().is_some_and(|identity| !identity.is_valid())
                || query
                    .native_identity
                    .as_ref()
                    .zip(plan.native_identity.as_ref())
                    .is_some_and(|(query, plan)| query != &plan.query)
            {
                return Err(invalid());
            }
            observed_rows += 1;
            match availability {
                None => availability = Some(summary.statistics().availability),
                Some(previous) if previous != summary.statistics().availability => mixed_availability = true,
                _ => (),
            }
            if observed_rows > MAX_RETAINED_ROWS {
                return Err(invalid());
            }
            let (native_fragment, native_fragments, execution_type, derived) = match summary {
                PageServerReadSummary::NativeFragment { native, .. } => {
                    if native.as_ref().is_some_and(|native| {
                        plan.native_identity
                            .as_ref()
                            .is_none_or(|plan| native.qualifiers.get("plan_id") != Some(&plan.plan_id))
                    }) {
                        return Err(invalid());
                    }
                    (
                        native.clone(),
                        "1".into(),
                        native.as_ref().and_then(|native| native.qualifiers.get("execution_type").cloned()),
                        None,
                    )
                }
                PageServerReadSummary::Derived { native_fragments, execution_type, .. } => (
                    None,
                    native_fragments.clone(),
                    execution_type.clone(),
                    Some((native_fragments.as_str(), execution_type.is_some())),
                ),
            };
            if entries.len() == MAX_DETAIL_ENTRIES {
                continue;
            }
            entries.push(PageServerReadEntry {
                identity: PageServerReadEntryIdentity::RecordedPlan {
                    query_label: query.id.clone(),
                    plan_label: plan.id.clone(),
                    native_plan: plan.native_identity.clone().map(Box::new),
                    native_fragment: native_fragment.map(Box::new),
                    native_fragments,
                    execution_type,
                },
                executions: summary.executions().into(),
                explanation: explain_statistics(summary.statistics(), derived)?,
                statement: None,
            });
        }
    }
    let read_complete = matches!(source.map(|source| source.state), Some(SourceState::Collected | SourceState::Empty));
    let mut limitations = common_limitations();
    let availability = if mixed_availability { None } else { availability };
    if let Some(qualification) = availability_qualification(availability) {
        limitations.push(qualification);
    }
    let presentation_limited = observed_rows > MAX_DETAIL_ENTRIES;
    if presentation_limited {
        limitations.push(t!(
            "view.page_server_reads.limit.presentation",
            displayed = format::digits(entries.len().to_string())
        ));
    }
    limitations.push(t!("view.page_server_reads.limit.scan"));
    if !read_complete {
        limitations.push(t!("view.page_server_reads.limit.read"));
    }
    Ok(PageServerReadReport {
        source: workload.source.clone(),
        availability,
        read_complete,
        selected_rows: None,
        observed_rows: observed_rows as u32,
        presentation_limited,
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.page_server_reads.title"),
        summary: t!("view.page_server_reads.summary.plans", observed = format::digits(observed_rows.to_string())),
        limitations,
        entries,
        runtime_read: None,
        reference_coverage: None,
        definition_read: None,
    })
}

fn explain_statistics(
    statistics: &PageServerReadStatistics,
    derived: Option<(&str, bool)>,
) -> Result<PageServerReadExplanation> {
    if !statistics.is_valid() {
        return Err(invalid());
    }
    let mut limitations = common_limitations();
    let mut metrics = Vec::new();
    if statistics.availability == PageServerReadAvailability::Available {
        for (label, value) in [
            (
                if derived.is_some() {
                    key!("view.page_server_reads.metric.derived_average")
                } else {
                    key!("view.page_server_reads.metric.average")
                },
                statistics.average_pages.map(|value| value.to_string()),
            ),
            (key!("view.page_server_reads.metric.last"), statistics.last_pages.clone()),
            (key!("view.page_server_reads.metric.minimum"), statistics.minimum_pages.clone()),
            (key!("view.page_server_reads.metric.maximum"), statistics.maximum_pages.clone()),
            (
                key!("view.page_server_reads.metric.deviation"),
                statistics.standard_deviation_pages.map(|value| value.to_string()),
            ),
        ] {
            metrics.push(Metric::said_text(
                label,
                value.map(format::digits).unwrap_or_else(|| t!("view.page_server_reads.unknown")),
            ));
        }
    }
    if let Some((fragments, one_outcome)) = derived {
        limitations.push(t!("view.page_server_reads.limit.derived", fragments = format::digits(fragments.into())));
        if !one_outcome {
            limitations.push(t!("view.page_server_reads.limit.outcomes"));
        }
    }
    Ok(PageServerReadExplanation {
        availability: statistics.availability,
        title: t!("view.page_server_reads.title"),
        summary: match statistics.availability {
            PageServerReadAvailability::Available if statistics.has_values() => t!("view.page_server_reads.observed"),
            PageServerReadAvailability::Available => t!("view.page_server_reads.native_null"),
            PageServerReadAvailability::NotApplicable => t!("view.page_server_reads.not_applicable"),
            PageServerReadAvailability::Unavailable => t!("view.page_server_reads.unavailable"),
            PageServerReadAvailability::Unknown => t!("view.page_server_reads.profile_unknown"),
        },
        metrics,
        limitations,
        investigate: t!("view.page_server_reads.investigate"),
        verify: t!("view.page_server_reads.verify"),
    })
}

fn common_limitations() -> Vec<String> {
    vec![
        t!("view.page_server_reads.limit.lifetime"),
        t!("view.page_server_reads.limit.storage"),
        t!("view.page_server_reads.limit.population"),
        t!("view.page_server_reads.limit.precision"),
        t!("view.page_server_reads.limit.origin"),
    ]
}

fn availability_qualification(availability: Option<PageServerReadAvailability>) -> Option<String> {
    match availability {
        Some(PageServerReadAvailability::Available) => None,
        Some(PageServerReadAvailability::NotApplicable) => Some(t!("view.page_server_reads.not_applicable")),
        Some(PageServerReadAvailability::Unavailable) => Some(t!("view.page_server_reads.unavailable")),
        Some(PageServerReadAvailability::Unknown) | None => Some(t!("view.page_server_reads.profile_unknown")),
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.page_server_reads.invalid"))
}

#[cfg(test)]
mod tests;
