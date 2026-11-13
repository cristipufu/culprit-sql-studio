//! Native wait meaning comes only from the selected authenticated capture. Sparse waits are lower bounds.
use super::{groups, limit};
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceFamily};
use culprit_core::{
    QueryStat, QueryWaitCoverage, WaitStat,
    collected_analysis::{CollectedAnalysisLimit, CollectedAnalysisLimitation},
    telemetry::*,
};
use std::collections::BTreeMap;

pub(super) type NativeWait<'a> = (&'a ExportedRecord, &'a WaitMeasurement);

pub(super) struct Projection<'a> {
    semantics: Option<&'a QueryWaitSemantics>,
    family_complete: bool,
    joins_complete: bool,
}
impl<'a> Projection<'a> {
    pub fn new(
        capture: &'a EvidenceCapture,
        joins_complete: bool,
        limitations: &mut Vec<CollectedAnalysisLimitation>,
    ) -> Self {
        let semantics = match capture.source().query_history().and_then(|history| history.waits.as_ref()) {
            None => {
                limitations.push(limit(None, CollectedAnalysisLimit::WaitSemanticsUnavailable));
                None
            }
            Some(semantics) => {
                match semantics.capture {
                    WaitCaptureState::Enabled => (),
                    WaitCaptureState::Disabled => {
                        limitations.push(limit(None, CollectedAnalysisLimit::WaitCaptureDisabled));
                    }
                    WaitCaptureState::Unavailable => {
                        limitations.push(limit(None, CollectedAnalysisLimit::WaitCaptureUnavailable));
                    }
                }
                if semantics.coverage == WaitCaptureCoverage::Unverified {
                    limitations.push(limit(None, CollectedAnalysisLimit::WaitIntervalCoverageUnverified));
                }
                // Current configuration cannot erase historical rows or qualify their recording interval.
                Some(semantics)
            }
        };
        // A selected, published empty batch is complete; an unselected or missing family is not an empty measure.
        let family_complete = capture.families().contains(&EvidenceFamily::QueryWaits)
            && !capture.gaps().iter().any(|gap| gap.stream.is_none_or(|stream| stream == TelemetryStream::QueryWaits));
        if !family_complete {
            limitations.push(limit(None, CollectedAnalysisLimit::WaitFamilyIncomplete));
        }
        Self { semantics, family_complete, joins_complete }
    }

    /// Unmapped or inexact native rows remain in the analysis context. Supported positives survive independently;
    /// their absence becomes measurable only when this exact native family and every relevant join are complete.
    pub fn apply(
        &self,
        query: &mut QueryStat,
        rows: &[NativeWait<'_>],
        limitations: &mut Vec<CollectedAnalysisLimitation>,
    ) -> bool {
        let Some(semantics) = self.semantics else { return !rows.is_empty() };
        let mut complete =
            semantics.coverage == WaitCaptureCoverage::Qualified && self.family_complete && self.joins_complete;
        let mut totals = BTreeMap::<&str, (WaitClass, WaitTotal)>::new();
        for (export, row) in rows {
            let class = match semantics.categories.get(&row.category).map(|meaning| meaning.disposition) {
                Some(WaitDisposition::Analyze(class)) => class,
                Some(WaitDisposition::Ignore) => continue,
                None | Some(WaitDisposition::Unknown) => {
                    complete = false;
                    limitations.push(limit(export.record_id.clone(), CollectedAnalysisLimit::UnknownWaitCategory));
                    continue;
                }
            };
            let (_, total) = totals.entry(class.key()).or_insert_with(|| (class, WaitTotal::default()));
            if !total.add(&row.total, row.unit) {
                complete = false;
                limitations.push(limit(export.record_id.clone(), CollectedAnalysisLimit::CounterPrecision));
            }
        }
        query.waits = totals
            .into_values()
            .filter_map(|(class, total)| {
                let wait_ms = total.milliseconds()?;
                (wait_ms > 0.0).then(|| WaitStat {
                    category: class.category().into(),
                    // A category is not an individual wait type. Exact native IDs and names remain in context.
                    wait_type: None,
                    wait_ms,
                    tasks: None,
                })
            })
            .collect();
        query.waits.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms).then_with(|| a.category.cmp(&b.category)));
        let classes = semantics
            .categories
            .values()
            .filter_map(|meaning| match meaning.disposition {
                WaitDisposition::Analyze(class) => Some((class.key(), class)),
                WaitDisposition::Ignore | WaitDisposition::Unknown => None,
            })
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect();
        query.waits_coverage = Some(QueryWaitCoverage { classes, complete });
        !complete
    }
}

#[derive(Default)]
struct WaitTotal(Vec<TimeUnitTotal>);

#[derive(Clone, Copy)]
struct TimeUnitTotal {
    unit: RuntimeMetricUnit,
    integer: u64,
    real: f64,
}
impl WaitTotal {
    /// Check exact integer sums before conversion; converting each fragment first can round MAX_EXACT + 1 down.
    /// Failed additions leave the already observed lower bound intact.
    fn add(&mut self, value: &MeasurementValue, unit: RuntimeMetricUnit) -> bool {
        let mut candidate = self.0.clone();
        let index = match candidate.iter().position(|total| total.unit == unit) {
            Some(index) => index,
            None => {
                candidate.push(TimeUnitTotal { unit, integer: 0, real: 0.0 });
                candidate.len() - 1
            }
        };
        let total = &mut candidate[index];
        match value {
            MeasurementValue::Integer(value) => {
                let Some(sum) = groups::exact_count(value)
                    .and_then(|value| total.integer.checked_add(value))
                    .filter(|sum| *sum <= groups::MAX_EXACT_COUNT)
                else {
                    return false;
                };
                total.integer = sum;
            }
            MeasurementValue::Real(value) => {
                let Some(sum) = checked_sum(total.real, *value) else { return false };
                total.real = sum;
            }
        }
        let next = Self(candidate);
        if next.milliseconds().is_none() {
            return false;
        }
        *self = next;
        true
    }

    fn milliseconds(&self) -> Option<f64> {
        self.0.iter().try_fold(0.0, |total, part| {
            let native = checked_sum(part.integer as f64, part.real)?;
            checked_sum(total, groups::milliseconds(native, part.unit)?)
        })
    }
}

fn checked_sum(left: f64, right: f64) -> Option<f64> {
    let total = left + right;
    (total.is_finite() && right >= 0.0 && (right == 0.0 || total > left)).then_some(total)
}
