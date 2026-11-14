use super::*;
use culprit_core::{
    DistributionPopulation, Engine, MeasurementScope, ResetMarker, RuntimeDistribution, RuntimeDistributionOrigin,
    RuntimeDuration, RuntimeHistogramBucket, RuntimeQuantile,
};

/// Independent native populations have different clocks/count qualification; no runtime or SQL row is invented.
pub fn runtime_distribution_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let histogram = RuntimeDistribution::native_histogram(
        "native-histogram",
        vec![
            RuntimeHistogramBucket {
                lower: RuntimeDuration::Picoseconds { value: "0".into() },
                upper: RuntimeDuration::Picoseconds { value: "2000000000".into() },
                samples: "970".into(),
            },
            RuntimeHistogramBucket {
                lower: RuntimeDuration::Picoseconds { value: "500000000000".into() },
                upper: RuntimeDuration::Picoseconds { value: "18446744073709551615".into() },
                samples: "30".into(),
            },
        ],
    );
    let summary = RuntimeDistribution {
        source: "native-summary".into(),
        origin: RuntimeDistributionOrigin::StatementSummary,
        population: DistributionPopulation::NativeCounters { sample_count: None },
        p50: None,
        p95: Some(RuntimeQuantile::HistogramUpperBound {
            lower: None,
            upper: RuntimeDuration::Picoseconds { value: "9007199254740993".into() },
        }),
        p99: Some(RuntimeQuantile::HistogramUpperBound {
            lower: None,
            upper: RuntimeDuration::Picoseconds { value: "18446744073709551615".into() },
        }),
        histogram: vec![],
        issue: None,
    };
    let retained = RuntimeDistribution {
        source: "retained-native-runs".into(),
        origin: RuntimeDistributionOrigin::CapturedRuns,
        population: DistributionPopulation::CapturedSamples { sample_count: "9007199254740993".into() },
        p50: Some(RuntimeQuantile::sample(1.0)),
        p95: Some(RuntimeQuantile::sample(3.0)),
        p99: Some(RuntimeQuantile::sample(4.0)),
        histogram: vec![],
        issue: None,
    };
    vec![
        frame(
            Engine::Mysql,
            vec![point(summary, "2020-01-01T12:00:00.100000000Z"), point(histogram, "2020-01-01T12:00:00.200000000Z")],
            vec![
                StateSourceCoverage::observed("native-summary".into(), 1, false),
                StateSourceCoverage::observed("native-histogram".into(), 1, false),
            ],
        ),
        frame(
            Engine::Postgres,
            vec![point(retained, "2020-01-01T12:00:00.300000000Z")],
            vec![
                StateSourceCoverage::collected("retained-native-runs".into(), 1),
                StateSourceCoverage::unavailable(
                    "native-histogram".into(),
                    culprit_core::ErrorCode::PermissionDenied,
                    None,
                ),
            ],
        ),
        frame(Engine::Mysql, vec![], vec![StateSourceCoverage::collected("native-histogram".into(), 0)]),
        frame(
            Engine::Mysql,
            vec![],
            vec![StateSourceCoverage::unavailable(
                "native-histogram".into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )],
        ),
    ]
}

fn point(distribution: RuntimeDistribution, observed_at: &str) -> TelemetryRecord {
    let point = StatementRuntimeDistribution {
        identity: NativeIdentity::new("opaque-digest").qualified("native-context", "source-owned-database"),
        scope: MeasurementScope::Database,
        observed_at: observed_at.into(),
        reset: ResetMarker::Unknown,
        distribution,
    };
    assert!(point.is_valid());
    TelemetryRecord::Operational(OperationalObservation {
        source: point.distribution.source.clone(),
        key: point.key().unwrap(),
        fact: OperationalFact::RuntimeDistribution(Box::new(point)),
    })
}

fn frame(
    engine: Engine,
    rows: Vec<TelemetryRecord>,
    sources: Vec<StateSourceCoverage>,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    (
        CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(engine, "db", "fixture.invalid", 3306, None).into(),
            source: "runtime-populations".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::RuntimeDistributions,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: rows.len() as u32,
                sources,
            }),
        },
        rows,
    )
}
