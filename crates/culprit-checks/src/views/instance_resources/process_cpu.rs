//! Native process totals and explicit original-pair CPU comparisons retain independent populations and clocks.
pub(super) mod interval;
mod profile;
use culprit_core::{Engine, InstanceResourceField, ProcessCpuReport, Result, Snapshot, SourceState, t, telemetry::*};
pub(super) use interval::explain as compare_originals;
pub(super) use profile::explain as explain_profile;

pub fn explain_source(
    source: &CollectionDescription,
    coverage: &StateSourceCoverage,
    observations: &[&OperationalObservation],
    read_complete: bool,
) -> Result<ProcessCpuReport> {
    let context = NativeReadContext::from_snapshot(source)?;
    if context.target.engine() != Engine::Mssql
        || source.observation.snapshot().is_none_or(|snapshot| {
            snapshot.dataset != SnapshotDataset::Operational || !snapshot.sources.iter().any(|value| value == coverage)
        })
        || coverage.source != culprit_core::source_id::PROCESSOR_TOPOLOGY
        || coverage.records > MAX_MSSQL_PROCESSOR_OBSERVATIONS
        || observations.len() > coverage.records as usize
        || read_complete && observations.len() != coverage.records as usize
        || !observations.is_empty() && !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
    {
        return Err(invalid());
    }
    let mut report = empty(coverage.state);
    report.context = Some(ObservationReadContext::Native { context: Box::new(context.clone()) });
    report.native_source = Some(coverage.clone());
    report.read_complete = Some(read_complete);
    if !read_complete && matches!(coverage.state, SourceState::Collected | SourceState::Partial) {
        report.source_state = SourceState::Partial;
    }
    let mut samples = Vec::new();
    for row in observations {
        if row.source != coverage.source || !row.is_valid() {
            return Err(invalid());
        }
        match &row.fact {
            OperationalFact::CounterSet(sample) => {
                MssqlProcessCpuPoint::from_original(sample, &context)?;
                samples.push(sample.as_ref());
            }
            OperationalFact::Counter(_) => (),
            _ => return Err(invalid()),
        }
    }
    fill(&mut report, &samples)?;
    qualify(&mut report);
    Ok(report)
}

pub(super) fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<ProcessCpuReport>> {
    let samples = snapshot
        .pressure
        .as_ref()
        .and_then(|pressure| pressure.resource_counters.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|sample| sample.identity.qualifiers.get("kind").map(String::as_str) == Some(MSSQL_PROCESS_CPU_KIND))
        .collect::<Vec<_>>();
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::PROCESSOR_TOPOLOGY);
    if samples.is_empty() && source.is_none() {
        return Ok(None);
    }
    if snapshot.server.engine != Engine::Mssql {
        return if samples.is_empty() { Ok(None) } else { Err(invalid()) };
    }
    if !samples.is_empty()
        && source.is_none_or(|source| !matches!(source.state, SourceState::Collected | SourceState::Partial))
    {
        return Err(invalid());
    }
    let mut report = empty(source.map_or(SourceState::Unavailable, |source| source.state));
    fill(&mut report, &samples)?;
    qualify(&mut report);
    Ok(Some(report))
}

fn fill(report: &mut ProcessCpuReport, samples: &[&NativeCounterSet]) -> Result<()> {
    if samples.len() > 1 {
        return Err(invalid());
    }
    let Some(sample) = samples.first() else { return Ok(()) };
    let point = MssqlProcessCpuPoint::from_sample(sample)?;
    report.observed_at = Some(point.observed_at().into());
    report.summary = if point.user_ms().is_none() && point.kernel_ms().is_none() {
        t!("view.process_cpu.summary.unavailable")
    } else {
        t!("view.process_cpu.summary.recorded")
    };
    report.fields = vec![
        field("process_id", t!("view.process_cpu.field.process_id"), Some(point.process_id())),
        field("user_time_ms", t!("view.process_cpu.field.user_ms"), point.user_ms()),
        field("kernel_time_ms", t!("view.process_cpu.field.kernel_ms"), point.kernel_ms()),
        field("started_ticks", t!("view.process_cpu.field.started_ticks"), Some(point.started_ticks())),
        field("sampled_ticks", t!("view.process_cpu.field.sampled_ticks"), Some(point.sampled_ticks())),
    ];
    Ok(())
}

fn field(key: &str, label: String, value: Option<&str>) -> InstanceResourceField {
    InstanceResourceField { key: key.into(), label, value: value.map(str::to_owned) }
}
fn empty(state: SourceState) -> ProcessCpuReport {
    ProcessCpuReport {
        capture: None,
        populations: Vec::new(),
        context: None,
        native_source: None,
        source_state: state,
        read_complete: None,
        observed_at: None,
        title: t!("view.process_cpu.title"),
        summary: t!("view.process_cpu.summary.unavailable"),
        limitations: vec![t!("view.process_cpu.limit.meaning"), t!("view.process_cpu.limit.comparison")],
        fields: vec![],
    }
}
pub(super) fn invalid_report() -> ProcessCpuReport {
    let mut report = empty(SourceState::Unavailable);
    report.limitations.push(t!("view.process_cpu.invalid"));
    report
}
fn qualify(report: &mut ProcessCpuReport) {
    if report.source_state == SourceState::Partial {
        report.limitations.push(t!("view.process_cpu.limit.partial"));
    }
    if report.observed_at.is_none() || report.fields.iter().any(|field| field.value.is_none()) {
        report.limitations.push(t!("view.process_cpu.limit.unavailable"));
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
