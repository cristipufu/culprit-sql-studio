//! A fraction declaration refers only to exact inputs retained together by its native source.
use super::*;
use std::collections::BTreeSet;

const MAX_RATIOS: usize = 32;
const MAX_REJECTED_INPUTS: usize = MAX_RATIOS * 2;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterRatioKind {
    LocalSsdCacheHit,
    PlanCacheHit,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterRatioSupport {
    Supported,
    Unsupported,
    Unknown,
}

/// The keys are a source's expectation, not proof that either native input was observed. An all-missing pair
/// remains a source-read gap with no invented counter object or instance.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeCounterRatio {
    pub kind: CounterRatioKind,
    pub numerator: Option<String>,
    pub base: Option<String>,
    pub support: CounterRatioSupport,
    /// Native UTC clock from the same fully drained counter statement, never the counter's reset time.
    pub observed_at: Option<String>,
}

/// An observed signed native value that cannot satisfy the existing unsigned measurement contract.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct CounterRatioGap {
    pub key: String,
    pub raw_value: String,
    pub native_object: String,
    pub native_name: String,
    pub native_instance: Option<String>,
    pub native_type: String,
}

impl CounterRatioGap {
    fn is_valid(&self) -> bool {
        [&self.key, &self.native_object, &self.native_name, &self.native_type].into_iter().all(|value| valid_key(value))
            && self.native_instance.as_deref().is_none_or(valid_key)
            && self.raw_value.parse::<i64>().ok().is_some_and(|value| value < 0 && value.to_string() == self.raw_value)
            && self.native_type.parse::<i32>().ok().is_some_and(|value| value.to_string() == self.native_type)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CounterRatioIssue {
    UnknownProfile,
    UnsupportedProfile,
    ReadClockUnavailable,
    MissingNumerator,
    MissingBase,
    InvalidNumerator,
    InvalidBase,
    UnsupportedCounterType,
    IncompatibleInputs,
    ZeroBase,
    OutsideSubset,
}

impl NativeCounterRatio {
    fn is_valid(&self) -> bool {
        self.numerator.as_deref().is_none_or(valid_key)
            && self.base.as_deref().is_none_or(valid_key)
            && (self.numerator.is_none() || self.base.is_none() || self.numerator != self.base)
            && self.observed_at.as_deref().is_none_or(|at| at.len() <= 64 && ObservationBounds::new(at, at).is_some())
    }

    /// Source-owned declarations and original integers are checked before any presentation calculation.
    pub fn issues(&self, sample: &NativeCounterSet) -> Vec<CounterRatioIssue> {
        use CounterRatioIssue::*;
        let mut issues = BTreeSet::new();
        if !sample.ratios.contains(self) {
            issues.insert(IncompatibleInputs);
        }
        match self.support {
            CounterRatioSupport::Supported => (),
            CounterRatioSupport::Unsupported => {
                issues.insert(UnsupportedProfile);
            }
            CounterRatioSupport::Unknown => {
                issues.insert(UnknownProfile);
            }
        }
        if self.observed_at.is_none() {
            issues.insert(ReadClockUnavailable);
        }
        let numerator = self.input(sample, self.numerator.as_deref(), MissingNumerator, InvalidNumerator, &mut issues);
        let base = self.input(sample, self.base.as_deref(), MissingBase, InvalidBase, &mut issues);
        if let (Some(numerator), Some(base)) = (numerator, base) {
            if numerator.nature != CounterNature::FractionNumerator
                || base.nature != CounterNature::FractionBase
                || numerator.unit != CounterUnit::Count
                || base.unit != CounterUnit::Count
                || numerator.native_type.is_none()
                || base.native_type.is_none()
            {
                issues.insert(UnsupportedCounterType);
            }
            if numerator.native_object != base.native_object
                || numerator.native_instance != base.native_instance
                || numerator.scope != base.scope
                || numerator.reset != base.reset
                || !matches!(
                    sample.scope,
                    crate::MeasurementScope::Server | crate::MeasurementScope::Pool | crate::MeasurementScope::Node
                )
            {
                issues.insert(IncompatibleInputs);
            }
            if let (Some(numerator), Some(base)) = (numerator.value.integer(), base.value.integer()) {
                if base == 0 {
                    issues.insert(ZeroBase);
                }
                if numerator > base {
                    issues.insert(OutsideSubset);
                }
            } else {
                issues.insert(IncompatibleInputs);
            }
        }
        issues.into_iter().collect()
    }

    fn input<'a>(
        &self,
        sample: &'a NativeCounterSet,
        key: Option<&str>,
        missing: CounterRatioIssue,
        invalid: CounterRatioIssue,
        issues: &mut BTreeSet<CounterRatioIssue>,
    ) -> Option<&'a NativeCounterSample> {
        let Some(key) = key else {
            issues.insert(missing);
            return None;
        };
        if sample.ratio_gaps.iter().any(|gap| gap.key == key) {
            issues.insert(invalid);
            return None;
        }
        let mut matching = sample.counters.iter().filter(|counter| counter.key == key);
        let input = matching.next();
        if matching.next().is_some() {
            issues.insert(CounterRatioIssue::IncompatibleInputs);
            return None;
        }
        if input.is_none() {
            issues.insert(missing);
        }
        input
    }

    /// Percentage to two decimal places, truncated using checked integer arithmetic. The raw inputs remain exact;
    /// this is an aggregate fraction, never a recent interval, rate, pressure grade or cache-miss diagnosis.
    pub fn percent(&self, sample: &NativeCounterSet) -> Option<String> {
        if !sample.is_valid() || !self.issues(sample).is_empty() {
            return None;
        }
        let numerator =
            sample.counters.iter().find(|counter| Some(&counter.key) == self.numerator.as_ref())?.value.integer()?;
        let base = sample.counters.iter().find(|counter| Some(&counter.key) == self.base.as_ref())?.value.integer()?;
        let hundredths = u128::from(numerator).checked_mul(10_000)?.checked_div(u128::from(base))?;
        Some(format!("{}.{:02}", hundredths / 100, hundredths % 100))
    }
}

pub(super) fn valid(sample: &NativeCounterSet) -> bool {
    let mut pairs = BTreeSet::new();
    let mut gaps = BTreeSet::new();
    sample.ratios.len() <= MAX_RATIOS
        && sample.ratio_gaps.len() <= MAX_REJECTED_INPUTS
        && sample
            .ratios
            .first()
            .is_none_or(|first| sample.ratios.iter().all(|ratio| ratio.observed_at == first.observed_at))
        && sample.ratios.iter().all(|ratio| {
            ratio.is_valid()
                && pairs.insert((ratio.kind, &ratio.numerator, &ratio.base))
                && [&ratio.numerator, &ratio.base].into_iter().flatten().all(|key| {
                    sample.counters.iter().any(|counter| &counter.key == key)
                        || sample.ratio_gaps.iter().any(|gap| &gap.key == key)
                })
        })
        && sample.ratio_gaps.iter().all(|gap| {
            gap.is_valid()
                && gaps.insert(&gap.key)
                && sample.counters.iter().all(|counter| counter.key != gap.key)
                && sample.ratios.iter().any(|ratio| {
                    ratio.numerator.as_deref() == Some(gap.key.as_str())
                        || ratio.base.as_deref() == Some(gap.key.as_str())
                })
        })
}

#[cfg(test)]
mod tests;
