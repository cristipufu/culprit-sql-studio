//! The original selected population is independent of a later scan's namespace, member and clocks.
use super::*;

pub fn explain_database_captured(read: &CapturedOperationalRead) -> Result<PerformanceCounterReport> {
    if !read.is_valid()
        || read
            .description
            .observation
            .snapshot()
            .is_none_or(|header| header.dataset != SnapshotDataset::DatabasePerformanceCounters)
    {
        return Err(invalid());
    }
    let coverage = read.source_coverage(culprit_core::source_id::PERFORMANCE_COUNTERS).ok_or_else(invalid)?;
    let rows = read.observations.iter().collect::<Vec<_>>();
    let mut report = explain_source(&read.description, coverage, &rows, read.read_complete)?;
    report.limitations.push(t!("view.performance_counters.limit.historical"));
    Ok(report)
}

pub fn explain_recorded(recorded: &RecordedPerformanceCounters) -> Result<PerformanceCounterReport> {
    match recorded {
        RecordedPerformanceCounters::Captured { read } => explain_database_captured(read),
        RecordedPerformanceCounters::Unavailable { reason } => {
            Ok(unavailable(PerformanceCounterPopulation::SelectedDatabase, reason))
        }
    }
}

pub fn unavailable(
    population: PerformanceCounterPopulation,
    reason: &culprit_core::i18n::Message,
) -> PerformanceCounterReport {
    PerformanceCounterReport {
        population,
        native_database_id: None,
        database: None,
        context: None,
        native_source: None,
        source_state: Some(SourceState::Unavailable),
        source_reason: Some(reason.render()),
        read_complete: None,
        native_observations: None,
        observed_observations: 0,
        native_counter_rows: None,
        observed_counter_rows: 0,
        presented_counter_rows: 0,
        entries_limited: false,
        fields_withheld: false,
        missing_fields: Vec::new(),
        title: t!("view.performance_counters.title"),
        summary: reason.render(),
        limitations: match population {
            PerformanceCounterPopulation::SelectedDatabase => vec![
                t!("view.performance_counters.limit.database"),
                t!("view.performance_counters.limit.historical"),
                t!("view.performance_counters.limit.original"),
            ],
            PerformanceCounterPopulation::SharedServer => {
                vec![t!("view.performance_counters.limit.scope"), t!("view.performance_counters.limit.original")]
            }
        },
        entries: Vec::new(),
        ratios: None,
    }
}
