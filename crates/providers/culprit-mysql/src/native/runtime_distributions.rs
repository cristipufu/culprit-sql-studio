//! Keep each native read's quantile population independent, including exact picosecond bounds and bucket counts.
//! Histogram resets are independent and digest-summary truncation also removes its histogram. Neither a native
//! statement clock nor retained positive bins establishes an atomic or continuous execution population.
//! https://dev.mysql.com/doc/refman/8.4/en/performance-schema-statement-histogram-summary-tables.html
use crate::{read::QueryRead, rows::Rec};
use culprit_core::{
    DistributionPopulation, MAX_RUNTIME_HISTOGRAM_BUCKETS, RuntimeDistribution, RuntimeDistributionIssue,
    RuntimeDistributionOrigin, RuntimeDuration, RuntimeHistogramBucket, RuntimeQuantile,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SUMMARY: &str = "performance_schema.events_statements_summary_by_digest";
pub(crate) const HISTOGRAM: &str = "performance_schema.events_statements_histogram_by_digest";
pub(crate) const MAX_DIGESTS: usize = 128;
pub(crate) const MAX_TOTAL_BINS: usize = 4096;

pub(crate) fn summary(row: &Rec) -> Option<RuntimeDistribution> {
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

pub(crate) fn histograms(rows: &[Rec], requested: &[String]) -> BTreeMap<String, RuntimeDistribution> {
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

pub(crate) fn unavailable(
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

pub(crate) struct HistogramObservation {
    pub digest: String,
    pub observed_at: String,
    pub distribution: RuntimeDistribution,
}

/// The same native statement and decoder serve scans and scheduled collection. The cap bounds returned rows;
/// it does not prove the cost of iterating a Performance Schema container under production concurrency.
pub(crate) async fn read_histograms<R: QueryRead>(
    reader: &mut R,
    database: &str,
    digests: &[String],
    max_bins: usize,
) -> culprit_core::Result<Vec<HistogramObservation>> {
    if digests.is_empty() {
        return Ok(Vec::new());
    }
    if digests.len() > MAX_DIGESTS || max_bins == 0 || max_bins > MAX_TOTAL_BINS {
        return Err(super::invalid());
    }
    let unique: BTreeSet<_> = digests.iter().collect();
    if unique.len() != digests.len() || digests.iter().any(|digest| !native_digest(digest)) {
        return Err(super::invalid());
    }
    let marks = vec!["?"; digests.len()].join(",");
    let sql = format!(
        "SELECT DIGEST AS digest, SCHEMA_NAME AS schema_name,
           DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at,
           BUCKET_TIMER_LOW AS low_ps, BUCKET_TIMER_HIGH AS high_ps, COUNT_BUCKET AS runs
         FROM {HISTOGRAM}
         WHERE SCHEMA_NAME=? AND DIGEST IN ({marks}) AND COUNT_BUCKET>0
         ORDER BY DIGEST, BUCKET_NUMBER LIMIT {}",
        max_bins + 1
    );
    let mut parameters = vec![mysql_async::Value::from(database)];
    parameters.extend(digests.iter().map(|digest| mysql_async::Value::from(digest.as_str())));
    let rows = reader.read(&sql, parameters, max_bins + 1).await?;
    if rows.len() > max_bins {
        return Err(culprit_core::telemetry::CollectionStop::RowLimit.error());
    }
    let mut clocks = BTreeMap::new();
    for row in &rows {
        if super::text(row, "schema_name")? != database {
            return Err(super::invalid());
        }
        let digest = super::text(row, "digest")?;
        let at = super::text(row, "observed_at")?;
        if culprit_core::telemetry::canonical_time(&at).is_err()
            || clocks.insert(digest, at.clone()).is_some_and(|previous| previous != at)
        {
            return Err(super::invalid());
        }
    }
    let decoded = histograms(&rows, digests);
    clocks
        .into_iter()
        .map(|(digest, observed_at)| {
            let distribution = decoded.get(&digest).ok_or_else(super::invalid)?.clone();
            Ok(HistogramObservation { digest, observed_at, distribution: valid_observation(distribution) })
        })
        .collect()
}

pub(crate) fn summary_projection(columns: &BTreeSet<String>) -> [String; 2] {
    ["QUANTILE_95", "QUANTILE_99"]
        .map(|column| if columns.contains(column) { column.into() } else { format!("NULL AS {column}") })
}

pub(crate) async fn read_summary<R: QueryRead>(
    reader: &mut R,
    database: &str,
    columns: &BTreeSet<String>,
    max_digests: usize,
) -> culprit_core::Result<Vec<Rec>> {
    if max_digests == 0 || max_digests > MAX_DIGESTS {
        return Err(super::invalid());
    }
    let quantiles = summary_projection(columns).join(",");
    reader
        .read(
            &format!(
                "SELECT DIGEST AS digest, SCHEMA_NAME AS schema_name,
             DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at,{quantiles}
             FROM {SUMMARY} WHERE SCHEMA_NAME=? AND DIGEST IS NOT NULL LIMIT {}",
                max_digests + 1,
            ),
            vec![mysql_async::Value::from(database)],
            max_digests + 1,
        )
        .await
}

pub(crate) fn identity(row: &Rec, database: &str) -> culprit_core::Result<culprit_core::telemetry::NativeIdentity> {
    if super::text(row, "schema_name")? != database {
        return Err(super::invalid());
    }
    let digest = super::text(row, "digest")?;
    if !native_digest(&digest) {
        return Err(super::invalid());
    }
    Ok(culprit_core::telemetry::NativeIdentity::new(digest).qualified("schema", database))
}

pub(crate) fn native_digest(value: &str) -> bool {
    matches!(value.len(), 32 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Malformed native values remain an explicitly unusable point, without exporting an invalid typed population.
pub(crate) fn valid_observation(distribution: RuntimeDistribution) -> RuntimeDistribution {
    if distribution.is_valid() {
        distribution
    } else {
        RuntimeDistribution::unavailable(
            &distribution.source,
            distribution.origin,
            distribution.issue.unwrap_or(RuntimeDistributionIssue::InvalidNativeValues),
        )
    }
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

    fn timed_row(digest: &str, schema: &str, observed_at: &str, low: u64, high: u64, samples: u64) -> Rec {
        Rec::from_values(vec![
            ("digest", digest.into()),
            ("schema_name", schema.into()),
            ("observed_at", observed_at.into()),
            ("low_ps", low.into()),
            ("high_ps", high.into()),
            ("runs", samples.into()),
        ])
    }

    #[tokio::test]
    async fn the_shared_native_reader_preserves_its_own_clock_exact_bounds_and_parameterized_scope() {
        let digest = "0123456789abcdef0123456789abcdef";
        let at = "2020-01-01T12:00:00.123456Z";
        let mut reader = crate::read::test_support::Read::with(vec![Ok(vec![timed_row(
            digest,
            "db",
            at,
            0,
            u64::MAX,
            9_007_199_254_740_993,
        )])]);
        let points = read_histograms(&mut reader, "db", &[digest.into()], 512).await.unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].observed_at, at);
        assert_eq!(points[0].distribution.histogram[0].samples, "9007199254740993");
        let (sql, parameters, limit) = &reader.queries[0];
        assert!(sql.contains("UTC_TIMESTAMP(6)") && sql.contains("LIMIT 513"));
        assert!(!sql.contains(digest));
        assert_eq!(parameters, &vec![Value::from("db"), Value::from(digest)]);
        assert_eq!(*limit, 513);
    }

    #[tokio::test]
    async fn native_row_excess_scope_mismatch_and_inconsistent_clocks_cannot_create_a_population() {
        let digest = "0123456789abcdef0123456789abcdef";
        for shape in ["rows", "schema", "clock", "digest"] {
            let mut rows = vec![timed_row(digest, "db", "2020-01-01T12:00:00.1Z", 0, 1, 1)];
            match shape {
                "rows" => rows.push(timed_row(digest, "db", "2020-01-01T12:00:00.1Z", 1, 2, 1)),
                "schema" => rows = vec![timed_row(digest, "other-db", "2020-01-01T12:00:00.1Z", 0, 1, 1)],
                "clock" => rows.push(timed_row(digest, "db", "2020-01-01T12:00:00.2Z", 1, 2, 1)),
                "digest" => {
                    rows = vec![timed_row("abcdef0123456789abcdef0123456789", "db", "2020-01-01T12:00:00.1Z", 0, 1, 1)]
                }
                _ => unreachable!(),
            }
            let mut reader = crate::read::test_support::Read::with(vec![Ok(rows)]);
            assert!(
                read_histograms(&mut reader, "db", &[digest.into()], if shape == "rows" { 1 } else { 512 })
                    .await
                    .is_err(),
                "{shape}"
            );
            assert_eq!(reader.queries.len(), 1);
        }
    }

    #[tokio::test]
    async fn an_empty_native_read_does_not_invent_a_clock_or_a_zero_count_population() {
        let digest = "0123456789abcdef0123456789abcdef";
        let mut reader = crate::read::test_support::Read::with(vec![Ok(vec![])]);
        assert!(read_histograms(&mut reader, "db", &[digest.into()], 512).await.unwrap().is_empty());
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
