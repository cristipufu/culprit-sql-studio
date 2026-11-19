//! Approximate elapsed work from matched cumulative means, after the comparison owner admits identity and clocks.
use culprit_core::{
    counter_rates::AdmittedCounterRate,
    format,
    telemetry::{CompilationCounterPoint, MssqlQueryCompilation, NativeReadContext, ObservationBounds, canonical_time},
};
use std::time::Duration;

const MAX_EXACT_INTEGER: u64 = 1 << 53;
const ROUNDING_OPERATIONS: f64 = 8.0;
const MAX_RELATIVE_ROUNDING_ERROR: f64 = 0.001;
const MIN_DISPLAYABLE_MICROSECONDS: f64 = 1.0;

pub(super) struct CompileWork {
    microseconds: f64,
    bracket_seconds: f64,
    rate: Option<AdmittedCounterRate>,
    material: bool,
}

pub(super) fn from_endpoints(
    before: &NativeReadContext,
    after: &NativeReadContext,
    baseline: &CompilationCounterPoint,
    current: &MssqlQueryCompilation,
    delta: u64,
) -> Option<CompileWork> {
    let before_count = baseline.compilations.parse::<u64>().ok()?;
    let after_count = current.compilations.parse::<u64>().ok()?;
    if delta == 0
        || baseline.reset != current.reset
        || after_count.checked_sub(before_count) != Some(delta)
        || current.seconds_since_first_compile < 0
        || canonical_time(&current.last_compile_at).ok()? <= canonical_time(&before.observed_until).ok()?
    {
        return None;
    }
    let before_total = cumulative_elapsed(before_count, baseline.average_duration_us)?;
    let after_total = cumulative_elapsed(after_count, current.average_duration_us)?;
    let microseconds = after_total - before_total;
    let rounding = (after_total.abs() + before_total.abs()) * f64::EPSILON * ROUNDING_OPERATIONS;
    if !microseconds.is_finite()
        || microseconds < MIN_DISPLAYABLE_MICROSECONDS
        || rounding > microseconds * MAX_RELATIVE_ROUNDING_ERROR
    {
        return None;
    }
    let bracket_seconds = format::epoch_seconds(&after.observed_until)? - format::epoch_seconds(&before.observed_from)?;
    if !bracket_seconds.is_finite() || bracket_seconds <= 0.0 {
        return None;
    }
    let rate = admitted_rate(before, after, delta, bracket_seconds);
    Some(CompileWork {
        microseconds,
        bracket_seconds,
        rate,
        material: super::super::policy::has_material_compile_cost(
            delta,
            microseconds / delta as f64,
            microseconds / 1000.0,
        ),
    })
}

fn cumulative_elapsed(count: u64, average: f64) -> Option<f64> {
    if count == 0 || count > MAX_EXACT_INTEGER || !average.is_finite() || average < 0.0 {
        return None;
    }
    let total = count as f64 * average;
    (total.is_finite() && total <= MAX_EXACT_INTEGER as f64).then_some(total)
}

fn admitted_rate(
    before: &NativeReadContext,
    after: &NativeReadContext,
    count: u64,
    bracket_seconds: f64,
) -> Option<AdmittedCounterRate> {
    let minimum = Duration::from_secs_f64(culprit_core::RATE_MIN_HOURS * 3600.0);
    let gap = ObservationBounds::new(&before.observed_until, &after.observed_from)?;
    if gap.duration_at_most(minimum.checked_sub(Duration::from_nanos(1))?) {
        return None;
    }
    AdmittedCounterRate::new(count as f64, bracket_seconds)
}

impl CompileWork {
    pub(super) fn milliseconds(&self) -> f64 {
        self.microseconds / 1000.0
    }
    pub(super) fn bracket_milliseconds(&self) -> f64 {
        self.bracket_seconds * 1000.0
    }
    pub(super) fn hourly_rate(&self) -> Option<f64> {
        self.rate.map(|rate| rate.per_second() * 3600.0)
    }
    pub(super) fn is_material(&self) -> bool {
        self.material
    }
}
