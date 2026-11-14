//! Quantiles retain their native bounds and population. Independent reads never borrow statement extrema or counts.
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use ts_rs::TS;

pub const MAX_RUNTIME_DISTRIBUTIONS: usize = 8;
pub const MAX_RUNTIME_HISTOGRAM_BUCKETS: usize = 512;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "unit", rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeDuration {
    Milliseconds { value: f64 },
    Picoseconds { value: String },
}
impl RuntimeDuration {
    pub fn milliseconds(&self) -> Option<f64> {
        let value = match self {
            Self::Milliseconds { value } => *value,
            Self::Picoseconds { value } => exact_count(value)? as f64 / 1_000_000_000.0,
        };
        (value.is_finite() && value >= 0.0).then_some(value)
    }
    fn compare(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Milliseconds { value: left }, Self::Milliseconds { value: right }) => {
                self.milliseconds()?;
                other.milliseconds()?;
                left.partial_cmp(right)
            }
            (Self::Picoseconds { value: left }, Self::Picoseconds { value: right }) => {
                Some(exact_count(left)?.cmp(&exact_count(right)?))
            }
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeQuantile {
    /// A nearest-rank value of the captured sample, not a claim about omitted executions.
    SampleValue { value: RuntimeDuration },
    /// The native bucket's high estimate. A missing lower bound remains unknown.
    HistogramUpperBound { lower: Option<RuntimeDuration>, upper: RuntimeDuration },
}
impl RuntimeQuantile {
    pub fn sample(milliseconds: f64) -> Self {
        Self::SampleValue { value: RuntimeDuration::Milliseconds { value: milliseconds } }
    }
    pub fn lower_ms(&self) -> Option<f64> {
        self.lower()?.milliseconds()
    }
    pub fn upper_ms(&self) -> Option<f64> {
        self.upper().milliseconds()
    }
    fn lower(&self) -> Option<&RuntimeDuration> {
        match self {
            Self::SampleValue { value } => Some(value),
            Self::HistogramUpperBound { lower, .. } => lower.as_ref(),
        }
    }
    fn upper(&self) -> &RuntimeDuration {
        match self {
            Self::SampleValue { value } => value,
            Self::HistogramUpperBound { upper, .. } => upper,
        }
    }
    pub fn is_valid(&self) -> bool {
        self.upper_ms().is_some()
            && match self {
                Self::SampleValue { .. } => true,
                Self::HistogramUpperBound { lower: None, .. } => true,
                Self::HistogramUpperBound { lower: Some(lower), upper } => lower.compare(upper) == Some(Ordering::Less),
            }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RuntimeHistogramBucket {
    pub lower: RuntimeDuration,
    pub upper: RuntimeDuration,
    /// Exact native or captured-sample count, never a rounded JavaScript number.
    pub samples: String,
}
impl RuntimeHistogramBucket {
    pub fn is_valid(&self) -> bool {
        self.lower.compare(&self.upper) == Some(Ordering::Less)
            && exact_count(&self.samples).is_some_and(|count| count > 0)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeDistributionOrigin {
    StatementSummary,
    NativeHistogram,
    CapturedRuns,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum DistributionPopulation {
    /// Only these retained rows, with logging/filter/retention coverage stated separately by their source.
    CapturedSamples { sample_count: String },
    /// No continuity or equality with statement counters, their reset, extrema or observation time is established.
    NativeCounters { sample_count: Option<String> },
}
impl DistributionPopulation {
    pub fn sample_count(&self) -> Option<u64> {
        match self {
            Self::CapturedSamples { sample_count } => exact_count(sample_count),
            Self::NativeCounters { sample_count } => exact_count(sample_count.as_deref()?),
        }
    }
    fn is_valid(&self) -> bool {
        match self {
            Self::CapturedSamples { sample_count } => exact_count(sample_count).is_some(),
            Self::NativeCounters { sample_count } => {
                sample_count.as_deref().is_none_or(|value| exact_count(value).is_some())
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeDistributionIssue {
    ReadUnavailable,
    InvalidNativeValues,
    InconsistentPopulation,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RuntimeDistribution {
    /// Declared native source identifier; never a translated label or a selector for analysis behavior.
    pub source: String,
    pub origin: RuntimeDistributionOrigin,
    pub population: DistributionPopulation,
    pub p50: Option<RuntimeQuantile>,
    pub p95: Option<RuntimeQuantile>,
    pub p99: Option<RuntimeQuantile>,
    pub histogram: Vec<RuntimeHistogramBucket>,
    /// Independent valid observations remain available when another source cannot be read or decoded.
    pub issue: Option<RuntimeDistributionIssue>,
}
impl RuntimeDistribution {
    pub fn unavailable(source: &str, origin: RuntimeDistributionOrigin, issue: RuntimeDistributionIssue) -> Self {
        Self {
            source: source.into(),
            origin,
            population: DistributionPopulation::NativeCounters { sample_count: None },
            p50: None,
            p95: None,
            p99: None,
            histogram: Vec::new(),
            issue: Some(issue),
        }
    }
    /// Quantiles are derived only from this exact native bucket population, with integer ranks and original bounds.
    pub fn native_histogram(source: &str, histogram: Vec<RuntimeHistogramBucket>) -> Self {
        let samples = histogram_samples(&histogram);
        let mut observed = Self {
            source: source.into(),
            origin: RuntimeDistributionOrigin::NativeHistogram,
            population: DistributionPopulation::NativeCounters { sample_count: samples.map(|value| value.to_string()) },
            p50: percentile_bucket(&histogram, 50),
            p95: percentile_bucket(&histogram, 95),
            p99: percentile_bucket(&histogram, 99),
            histogram,
            issue: None,
        };
        if !observed.is_valid() {
            observed.issue = Some(RuntimeDistributionIssue::InconsistentPopulation);
        }
        observed
    }
    pub fn is_valid(&self) -> bool {
        let source_valid =
            !self.source.trim().is_empty() && self.source.len() <= 256 && !self.source.chars().any(char::is_control);
        let origin_matches = matches!(
            (&self.origin, &self.population),
            (RuntimeDistributionOrigin::CapturedRuns, DistributionPopulation::CapturedSamples { .. })
                | (
                    RuntimeDistributionOrigin::StatementSummary | RuntimeDistributionOrigin::NativeHistogram,
                    DistributionPopulation::NativeCounters { .. }
                )
        );
        if !source_valid
            || !origin_matches
            || !self.population.is_valid()
            || self.histogram.len() > MAX_RUNTIME_HISTOGRAM_BUCKETS
        {
            return false;
        }
        let values = [&self.p50, &self.p95, &self.p99];
        let mut previous: Option<&RuntimeQuantile> = None;
        for quantile in values.into_iter().flatten() {
            if !quantile.is_valid()
                || !matches!(
                    (self.origin, quantile),
                    (RuntimeDistributionOrigin::CapturedRuns, RuntimeQuantile::SampleValue { .. })
                        | (
                            RuntimeDistributionOrigin::StatementSummary | RuntimeDistributionOrigin::NativeHistogram,
                            RuntimeQuantile::HistogramUpperBound { .. }
                        )
                )
                || previous.is_some_and(|prior| {
                    !matches!(prior.upper().compare(quantile.upper()), Some(Ordering::Less | Ordering::Equal))
                })
            {
                return false;
            }
            previous = Some(quantile);
        }
        if self.population.sample_count() == Some(0) && previous.is_some() {
            return false;
        }
        if self.histogram.is_empty() {
            return self.origin != RuntimeDistributionOrigin::NativeHistogram || previous.is_none();
        }
        let Some(samples) = histogram_samples(&self.histogram) else { return false };
        if self.population.sample_count() != Some(samples) {
            return false;
        }
        if self.origin == RuntimeDistributionOrigin::StatementSummary {
            return false;
        }
        if self.issue.is_some() {
            return true;
        }
        [(&self.p50, 50), (&self.p95, 95), (&self.p99, 99)].into_iter().all(|(observed, rank)| {
            let bucket = percentile_bucket(&self.histogram, rank);
            match (self.origin, observed, bucket) {
                (RuntimeDistributionOrigin::NativeHistogram, observed, expected) => *observed == expected,
                (RuntimeDistributionOrigin::CapturedRuns, None, _) => true,
                (
                    RuntimeDistributionOrigin::CapturedRuns,
                    Some(RuntimeQuantile::SampleValue { value }),
                    Some(RuntimeQuantile::HistogramUpperBound { lower: Some(lower), upper }),
                ) => {
                    matches!(lower.compare(value), Some(Ordering::Less | Ordering::Equal))
                        && matches!(value.compare(&upper), Some(Ordering::Less | Ordering::Equal))
                }
                _ => false,
            }
        })
    }
    pub fn is_usable(&self) -> bool {
        self.issue.is_none() && self.is_valid()
    }
}

fn exact_count(value: &str) -> Option<u64> {
    let count = value.parse::<u64>().ok()?;
    (count.to_string() == value).then_some(count)
}
fn histogram_samples(histogram: &[RuntimeHistogramBucket]) -> Option<u64> {
    if histogram.len() > MAX_RUNTIME_HISTOGRAM_BUCKETS {
        return None;
    }
    let mut total = 0u64;
    let mut previous: Option<&RuntimeDuration> = None;
    for bucket in histogram {
        if !bucket.is_valid()
            || previous.is_some_and(|end| !matches!(end.compare(&bucket.lower), Some(Ordering::Less | Ordering::Equal)))
        {
            return None;
        }
        total = total.checked_add(exact_count(&bucket.samples)?)?;
        previous = Some(&bucket.upper);
    }
    Some(total)
}
fn percentile_bucket(histogram: &[RuntimeHistogramBucket], percentile: u8) -> Option<RuntimeQuantile> {
    let samples = histogram_samples(histogram)?;
    if samples == 0 {
        return None;
    }
    let rank = (u128::from(samples) * u128::from(percentile)).div_ceil(100);
    let mut count = 0u128;
    for bucket in histogram {
        count += u128::from(exact_count(&bucket.samples)?);
        if count >= rank {
            return Some(RuntimeQuantile::HistogramUpperBound {
                lower: Some(bucket.lower.clone()),
                upper: bucket.upper.clone(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests;
