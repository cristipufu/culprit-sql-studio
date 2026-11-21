//! Three native populations preserve independent availability, counts and sample clocks.
use super::*;
use culprit_core::{InstanceResourceGroup, ProcessCpuPopulationReport, format};

pub(in crate::views::instance_resources) fn explain(
    source: &CollectionDescription,
    observations: &[OperationalObservation],
    read_complete: bool,
) -> Result<ProcessCpuReport> {
    if !cpu_profile_header_is_valid(source) {
        return Err(invalid());
    }
    let header = source.observation.snapshot().ok_or_else(invalid)?;
    let context = NativeReadContext::from_snapshot(source)?;
    let mut keys = std::collections::BTreeSet::new();
    for row in observations {
        if !cpu_profile_observation_fits(row, source) || !keys.insert((row.source.as_str(), row.key.as_str())) {
            return Err(invalid());
        }
        let coverage = header.sources.iter().find(|coverage| coverage.source == row.source).ok_or_else(invalid)?;
        if !matches!(coverage.state, SourceState::Collected | SourceState::Partial) {
            return Err(invalid());
        }
    }
    let mut report = empty(SourceState::Collected);
    report.context = Some(ObservationReadContext::Native { context: Box::new(context) });
    report.read_complete = Some(read_complete);
    let mut totals = Vec::new();
    for row in observations.iter().filter(|row| row.source == culprit_core::source_id::PROCESSOR_TOPOLOGY) {
        if let OperationalFact::CounterSet(sample) = &row.fact {
            totals.push(sample.as_ref());
        }
    }
    fill(&mut report, &totals)?;
    for name in [culprit_core::source_id::PROCESSOR_TOPOLOGY, MSSQL_CPU_POOLS_SOURCE, MSSQL_CPU_HISTORY_SOURCE] {
        let coverage = header.sources.iter().find(|coverage| coverage.source == name).ok_or_else(invalid)?;
        let rows = observations.iter().filter(|row| row.source == name).collect::<Vec<_>>();
        if rows.len() > coverage.records as usize || read_complete && rows.len() != coverage.records as usize {
            return Err(invalid());
        }
        let state = if !read_complete && matches!(coverage.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            coverage.state
        };
        if !matches!(state, SourceState::Collected | SourceState::Empty) {
            report.source_state = SourceState::Partial;
        }
        let (title, entries, at) = if name == MSSQL_CPU_POOLS_SOURCE {
            let entries = rows.iter().map(|row| pool_entry(row)).collect::<Result<Vec<_>>>()?;
            let at = rows.first().and_then(|row| match &row.fact {
                OperationalFact::CounterSet(sample) => {
                    MssqlCpuPoolPoint::from_sample(sample).ok().map(|point| point.observed_at().to_owned())
                }
                _ => None,
            });
            (t!("view.process_cpu.population.pools"), entries, at)
        } else if name == MSSQL_CPU_HISTORY_SOURCE {
            let entries = rows.iter().map(|row| history_entry(row)).collect::<Result<Vec<_>>>()?;
            let at = rows.first().and_then(|row| match &row.fact {
                OperationalFact::CounterSet(sample) => {
                    MssqlCpuHistoryPoint::from_sample(sample).ok().map(|point| point.observed_at().to_owned())
                }
                _ => None,
            });
            (t!("view.process_cpu.population.history"), entries, at)
        } else {
            continue;
        };
        let summary = if matches!(state, SourceState::Unavailable | SourceState::NotApplicable) {
            t!("view.process_cpu.population.unavailable")
        } else {
            t!("view.process_cpu.population.recorded", count = format::digits(rows.len().to_string()))
        };
        report.populations.push(ProcessCpuPopulationReport {
            native_source: coverage.clone(),
            source_state: state,
            read_complete,
            observed_at: at,
            native_rows: coverage.records,
            observed_rows: rows.len() as u32,
            title,
            summary,
            limitations: vec![
                t!("view.process_cpu.limit.original_point"),
                if name == MSSQL_CPU_POOLS_SOURCE {
                    t!("view.process_cpu.limit.pool_reset")
                } else {
                    t!("view.process_cpu.limit.native_sample")
                },
            ],
            entries,
        });
    }
    report.native_source =
        header.sources.iter().find(|source| source.source == culprit_core::source_id::PROCESSOR_TOPOLOGY).cloned();
    report.summary = t!("view.process_cpu.profile.recorded");
    report.limitations = vec![
        t!("view.process_cpu.limit.original_point"),
        t!("view.process_cpu.limit.no_other"),
        t!("view.process_cpu.limit.two_captures"),
    ];
    if !read_complete {
        report.limitations.push(t!("view.instance_resources.limit.read"));
    }
    if observations.is_empty() {
        report.source_state = SourceState::Unavailable;
        report.summary = t!("view.process_cpu.summary.unavailable");
    }
    Ok(report)
}
fn pool_entry(row: &OperationalObservation) -> Result<InstanceResourceGroup> {
    let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
    let point = MssqlCpuPoolPoint::from_sample(sample)?;
    Ok(InstanceResourceGroup {
        key: row.key.clone(),
        title: t!("view.process_cpu.pool.title", id = point.pool_id()),
        fields: vec![
            field("pool_id", t!("view.process_cpu.field.pool_id"), Some(point.pool_id())),
            field("total_cpu_usage_ms", t!("view.process_cpu.field.pool_cpu_ms"), point.total_ms()),
            field(
                "statistics_started_without_offset",
                t!("view.process_cpu.field.pool_reset"),
                Some(point.statistics_started()),
            ),
            field("sample_ms_ticks", t!("view.process_cpu.field.sampled_ticks"), Some(point.sample_ticks())),
            field(
                "host_logical_processors",
                t!("view.process_cpu.field.host_processors"),
                Some(point.host_processors()),
            ),
        ],
    })
}
fn history_entry(row: &OperationalObservation) -> Result<InstanceResourceGroup> {
    let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
    let point = MssqlCpuHistoryPoint::from_sample(sample)?;
    let other_process = point.other_process_percent().map(|value| value.to_string());
    Ok(InstanceResourceGroup {
        key: row.key.clone(),
        title: t!("view.process_cpu.population.history"),
        fields: vec![
            field("record_ms_ticks", t!("view.process_cpu.field.record_ticks"), Some(point.record_ticks())),
            field("sample_ms_ticks", t!("view.process_cpu.field.sampled_ticks"), Some(point.sample_ticks())),
            field("host_platform", t!("view.process_cpu.field.platform"), point.platform()),
            field("process_cpu_percent", t!("view.process_cpu.field.native_process_percent"), point.process_percent()),
            field("idle_cpu_percent", t!("view.process_cpu.field.native_idle_percent"), point.idle_percent()),
            field(
                "other_process_cpu_percent",
                t!("view.process_cpu.field.other_process_percent"),
                other_process.as_deref(),
            ),
            field("raw_process_utilization", t!("view.process_cpu.field.raw_process"), point.raw_process()),
            field("raw_system_idle", t!("view.process_cpu.field.raw_idle"), point.raw_idle()),
        ],
    })
}
