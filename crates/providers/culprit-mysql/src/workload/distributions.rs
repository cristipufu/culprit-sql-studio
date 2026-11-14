//! Keep each native read's quantile population independent, including exact picosecond bounds and bucket counts.
use crate::rows::Rec;
use culprit_core::{
    DistributionPopulation, MAX_RUNTIME_HISTOGRAM_BUCKETS, RuntimeDistribution, RuntimeDistributionIssue,
    RuntimeDistributionOrigin, RuntimeDuration, RuntimeHistogramBucket, RuntimeQuantile,
};
use std::collections::{BTreeMap, BTreeSet};

const SUMMARY: &str = "performance_schema.events_statements_summary_by_digest";
pub(super) const HISTOGRAM: &str = "performance_schema.events_statements_histogram_by_digest";

pub(super) fn summary(row: &Rec) -> Option<RuntimeDistribution> {
    let mut issue = None;
    let mut bound = |field: &str| {
        if !row.has_column(field) {
            return None;
        }
        match row.unsigned(field) {
            Ok(value) => value.map(|value| RuntimeQuantile::HistogramUpperBound {
                lower: None,
                upper: RuntimeDuration::Picoseconds { value: value.to_string() },
            }),
            Err(_) => {
                issue = Some(RuntimeDistributionIssue::InvalidNativeValues);
                None
            }
        }
    };
    let (p95, p99) = (bound("quantile_95"), bound("quantile_99"));
    if p95.is_none() && p99.is_none() && issue.is_none() {
        return None;
    }
    let mut observed = RuntimeDistribution {
        source: SUMMARY.into(),
        origin: RuntimeDistributionOrigin::StatementSummary,
        population: DistributionPopulation::NativeCounters { sample_count: None },
        p50: None,
        p95,
        p99,
        histogram: Vec::new(),
        issue,
    };
    if !observed.is_valid() {
        observed.issue = Some(RuntimeDistributionIssue::InconsistentPopulation);
    }
    Some(observed)
}

fn bucket(row: &Rec) -> culprit_core::Result<RuntimeHistogramBucket> {
    Ok(RuntimeHistogramBucket {
        lower: RuntimeDuration::Picoseconds { value: crate::native::integer(row, "low_ps")?.to_string() },
        upper: RuntimeDuration::Picoseconds { value: crate::native::integer(row, "high_ps")?.to_string() },
        samples: crate::native::integer(row, "runs")?.to_string(),
    })
}

pub(super) fn histograms(rows: &[Rec], requested: &[String]) -> BTreeMap<String, RuntimeDistribution> {
    let mut buckets: BTreeMap<_, Vec<RuntimeHistogramBucket>> =
        requested.iter().map(|id| (id.clone(), Vec::new())).collect();
    let mut invalid = BTreeSet::new();
    for row in rows {
        let Ok(Some(id)) = row.native_text("digest") else {
            return unavailable(requested, RuntimeDistributionIssue::InvalidNativeValues);
        };
        let Some(population) = buckets.get_mut(&id) else {
            return unavailable(requested, RuntimeDistributionIssue::InvalidNativeValues);
        };
        if population.len() >= MAX_RUNTIME_HISTOGRAM_BUCKETS {
            invalid.insert(id);
            continue;
        }
        match bucket(row) {
            Ok(value) => population.push(value),
            Err(_) => {
                invalid.insert(id);
            }
        }
    }
    buckets
        .into_iter()
        .map(|(id, buckets)| {
            let mut observed = RuntimeDistribution::native_histogram(HISTOGRAM, buckets);
            if invalid.contains(&id) {
                observed.issue = Some(RuntimeDistributionIssue::InvalidNativeValues);
                observed.p50 = None;
                observed.p95 = None;
                observed.p99 = None;
            }
            (id, observed)
        })
        .collect()
}

pub(super) fn unavailable(
    requested: &[String],
    issue: RuntimeDistributionIssue,
) -> BTreeMap<String, RuntimeDistribution> {
    requested
        .iter()
        .map(|id| {
            (id.clone(), RuntimeDistribution::unavailable(HISTOGRAM, RuntimeDistributionOrigin::NativeHistogram, issue))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql_async::Value;
    fn row(low: u64, high: u64, samples: u64) -> Rec {
        Rec::from_values(vec![
            ("digest", Value::from("query")),
            ("low_ps", Value::UInt(low)),
            ("high_ps", Value::UInt(high)),
            ("runs", Value::UInt(samples)),
        ])
    }
    #[test]
    fn summary_high_estimates_remain_unqualified_and_keep_native_integer_units() {
        let observed = summary(&Rec::from_values(vec![
            ("quantile_95", Value::UInt(9_007_199_254_740_993)),
            ("quantile_99", Value::UInt(u64::MAX)),
        ]))
        .unwrap();
        assert!(observed.is_usable());
        assert_eq!(observed.population.sample_count(), None);
        assert_eq!(
            observed.p99.unwrap(),
            RuntimeQuantile::HistogramUpperBound {
                lower: None,
                upper: RuntimeDuration::Picoseconds { value: u64::MAX.to_string() }
            }
        );
    }
    #[test]
    fn later_native_histogram_is_its_own_population_without_replacing_summary_values() {
        let summary = summary(&Rec::from_values(vec![("quantile_99", Value::UInt(900_000_000_000))])).unwrap();
        let observed =
            histograms(&[row(0, 2_000_000_000, 980), row(500_000_000_000, 1_000_000_000_000, 20)], &["query".into()])
                .remove("query")
                .unwrap();
        assert!(observed.is_usable());
        assert_eq!(observed.population.sample_count(), Some(1_000));
        assert_eq!(observed.p99.as_ref().unwrap().upper_ms(), Some(1_000.0));
        assert_eq!(summary.p99.as_ref().unwrap().upper_ms(), Some(900.0));
        assert_eq!(observed.histogram.len(), 2);
    }
    #[test]
    fn missing_histogram_and_malformed_summary_do_not_erase_the_other_source() {
        let summary = summary(&Rec::from_values(vec![
            ("quantile_95", Value::Double(1.0)),
            ("quantile_99", Value::UInt(900_000_000_000)),
        ]))
        .unwrap();
        assert_eq!(summary.issue, Some(RuntimeDistributionIssue::InvalidNativeValues));
        assert!(!summary.is_usable());
        assert_eq!(summary.p99.as_ref().unwrap().upper_ms(), Some(900.0));
        let missing = unavailable(&["query".into()], RuntimeDistributionIssue::ReadUnavailable);
        assert_eq!(missing["query"].issue, Some(RuntimeDistributionIssue::ReadUnavailable));
        let good = histograms(&[row(0, 2_000_000_000, 100)], &["query".into()]);
        assert!(good["query"].is_usable());
    }
    #[test]
    fn duplicate_overlapping_wrong_digest_and_noninteger_rows_are_unusable() {
        let id = ["query".into()];
        assert!(!histograms(&[row(0, 10, 50), row(0, 10, 50)], &id)["query"].is_usable());
        assert!(!histograms(&[Rec::from_values(vec![("digest", Value::from("foreign"))])], &id)["query"].is_usable());
        assert!(
            !histograms(
                &[Rec::from_values(vec![
                    ("digest", Value::from("query")),
                    ("low_ps", Value::UInt(0)),
                    ("high_ps", Value::UInt(10)),
                    ("runs", Value::Double(100.0))
                ])],
                &id
            )["query"]
                .is_usable()
        );
    }
}
