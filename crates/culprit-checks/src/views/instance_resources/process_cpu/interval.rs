//! Compare complete authenticated originals only; matching reset/capacity endpoints do not prove continuity.
use super::*;
use culprit_core::{ProcessCpuIntervalReport, format};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) enum CpuIntervalUnavailableReason {
    Selection,
    SameOriginal,
    Owner,
    Clock,
    Incomplete,
    Roster,
    Capacity,
    Reset,
    Counter,
}
impl CpuIntervalUnavailableReason {
    fn parameter(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::SameOriginal => "same_original",
            Self::Owner => "owner",
            Self::Clock => "clock",
            Self::Incomplete => "incomplete",
            Self::Roster => "roster",
            Self::Capacity => "capacity",
            Self::Reset => "reset",
            Self::Counter => "counter",
        }
    }
}

pub(in crate::views::instance_resources) fn explain(
    first: &CapturedOperationalRead,
    second: &CapturedOperationalRead,
) -> Result<ProcessCpuIntervalReport> {
    if !first.is_valid()
        || !second.is_valid()
        || !cpu_profile_header_is_valid(&first.description)
        || !cpu_profile_header_is_valid(&second.description)
    {
        return Err(invalid());
    }
    let a = first.context()?;
    let b = second.context()?;
    let mut report = unavailable(CpuIntervalUnavailableReason::Selection);
    report.first = Some(first.origin.capture.clone());
    report.second = Some(second.origin.capture.clone());
    report.first_context = Some(Box::new(a.clone()));
    report.second_context = Some(Box::new(b.clone()));
    let refusal = if first.origin.capture.capture == second.origin.capture.capture {
        Some(CpuIntervalUnavailableReason::SameOriginal)
    } else if a.target != b.target
        || a.partition != b.partition
        || !a.has_qualified_local_owner()
        || !b.has_qualified_local_owner()
    {
        Some(CpuIntervalUnavailableReason::Owner)
    } else if ObservationBounds::new(&a.observed_until, &b.observed_from).is_none() {
        Some(CpuIntervalUnavailableReason::Clock)
    } else {
        None
    };
    if let Some(reason) = refusal {
        report.summary = reason_text(reason);
        return Ok(report);
    }
    let (a, b) = match (Pools::from_original(first), Pools::from_original(second)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            report.summary = reason_text(CpuIntervalUnavailableReason::Incomplete);
            return Ok(report);
        }
    };
    if a.points.keys().ne(b.points.keys()) {
        report.summary = reason_text(CpuIntervalUnavailableReason::Roster);
        return Ok(report);
    }
    if a.processors != b.processors {
        report.summary = reason_text(CpuIntervalUnavailableReason::Capacity);
        return Ok(report);
    }
    let Some(elapsed) = b.ticks.checked_sub(a.ticks).filter(|elapsed| *elapsed > 0) else {
        report.summary = reason_text(CpuIntervalUnavailableReason::Clock);
        return Ok(report);
    };
    let mut numerator = 0u128;
    for (id, left) in &a.points {
        let right = &b.points[id];
        if left.statistics_started() != right.statistics_started() {
            report.summary = reason_text(CpuIntervalUnavailableReason::Reset);
            return Ok(report);
        }
        let (Some(left), Some(right)) = (left.total_ms().and_then(native), right.total_ms().and_then(native)) else {
            report.summary = reason_text(CpuIntervalUnavailableReason::Counter);
            return Ok(report);
        };
        let Some(delta) = right.checked_sub(left) else {
            report.summary = reason_text(CpuIntervalUnavailableReason::Counter);
            return Ok(report);
        };
        let Some(total) = numerator.checked_add(u128::from(delta)) else {
            report.summary = reason_text(CpuIntervalUnavailableReason::Counter);
            return Ok(report);
        };
        numerator = total;
    }
    let Some(capacity) = u128::from(elapsed).checked_mul(u128::from(a.processors)) else {
        report.summary = reason_text(CpuIntervalUnavailableReason::Capacity);
        return Ok(report);
    };
    if numerator > capacity || capacity == 0 {
        report.summary = reason_text(CpuIntervalUnavailableReason::Counter);
        return Ok(report);
    }
    let percent = numerator as f64 / capacity as f64 * 100.0;
    report.available = true;
    report.summary = t!(
        "view.process_cpu.interval.recorded",
        percent = format::percent_value(percent),
        processors = format::digits(a.processors.to_string()),
        elapsed = format::digits(elapsed.to_string())
    );
    report.fields = vec![
        // This derived decimal is approximate; the exact native numerator and denominator remain separate below.
        field("observed_average_cpu_percent", t!("view.process_cpu.field.average_percent"), Some(&percent.to_string())),
        field("engine_cpu_delta_ms", t!("view.process_cpu.field.delta_ms"), Some(&numerator.to_string())),
        field("native_elapsed_ms", t!("view.process_cpu.field.elapsed_ms"), Some(&elapsed.to_string())),
        field("host_logical_processors", t!("view.process_cpu.field.host_processors"), Some(&a.processors.to_string())),
        field("capacity_processor_ms", t!("view.process_cpu.field.capacity_ms"), Some(&capacity.to_string())),
    ];
    Ok(report)
}
struct Pools<'a> {
    points: BTreeMap<&'a str, MssqlCpuPoolPoint<'a>>,
    ticks: u64,
    processors: u64,
}
impl<'a> Pools<'a> {
    fn from_original(read: &'a CapturedOperationalRead) -> Result<Self> {
        let coverage = read.source_coverage(MSSQL_CPU_POOLS_SOURCE).ok_or_else(invalid)?;
        if !read.read_complete
            || coverage.state != SourceState::Collected
            || !coverage.complete_scope
            || coverage.records == 0
        {
            return Err(invalid());
        }
        let context = read.context()?;
        let mut points = BTreeMap::new();
        let mut cohort = None;
        for row in read.observations.iter().filter(|row| row.source == MSSQL_CPU_POOLS_SOURCE) {
            let OperationalFact::CounterSet(sample) = &row.fact else { return Err(invalid()) };
            let point = MssqlCpuPoolPoint::from_original(sample, &context)?;
            let sampled = (point.sample_ticks(), point.host_processors(), point.observed_at());
            if cohort.replace(sampled).is_some_and(|prior| prior != sampled) {
                return Err(invalid());
            }
            if points.insert(point.pool_id(), point).is_some() {
                return Err(invalid());
            }
        }
        if points.len() != coverage.records as usize {
            return Err(invalid());
        }
        let (ticks, processors, _) = cohort.ok_or_else(invalid)?;
        let ticks = native(ticks).ok_or_else(invalid)?;
        let processors = native(processors).filter(|n| *n > 0).ok_or_else(invalid)?;
        Ok(Self { points, ticks, processors })
    }
}
fn native(value: &str) -> Option<u64> {
    value.parse::<i64>().ok().filter(|value| *value >= 0).map(|value| value as u64)
}
fn reason_text(reason: CpuIntervalUnavailableReason) -> String {
    t!("view.process_cpu.interval.unavailable", reason = reason.parameter())
}
pub(in crate::views::instance_resources) fn unavailable(
    reason: CpuIntervalUnavailableReason,
) -> ProcessCpuIntervalReport {
    ProcessCpuIntervalReport {
        available: false,
        first: None,
        second: None,
        first_context: None,
        second_context: None,
        title: t!("view.process_cpu.interval.title"),
        summary: reason_text(reason),
        fields: Vec::new(),
        limitations: vec![
            t!("view.process_cpu.limit.observed_interval"),
            t!("view.process_cpu.limit.pool_reset"),
            t!("view.process_cpu.limit.no_other"),
        ],
    }
}
