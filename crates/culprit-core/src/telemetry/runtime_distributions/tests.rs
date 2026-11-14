use super::*;
use crate::{RuntimeDistributionOrigin, RuntimeDuration, RuntimeHistogramBucket};

fn sample() -> StatementRuntimeDistribution {
    StatementRuntimeDistribution {
        identity: NativeIdentity::new("native-digest").qualified("schema", "db"),
        scope: MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456Z".into(),
        reset: ResetMarker::Unknown,
        distribution: RuntimeDistribution::native_histogram(
            "native-histogram",
            vec![RuntimeHistogramBucket {
                lower: RuntimeDuration::Picoseconds { value: "0".into() },
                upper: RuntimeDuration::Picoseconds { value: u64::MAX.to_string() },
                samples: "9007199254740993".into(),
            }],
        ),
    }
}

#[test]
fn a_distribution_point_preserves_exact_native_units_counts_identity_and_unknown_reset() {
    let point = sample();
    assert!(point.is_valid());
    let json = serde_json::to_string(&point).unwrap();
    let restored: StatementRuntimeDistribution = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, point);
    assert_eq!(restored.reset, ResetMarker::Unknown);
    assert_eq!(restored.distribution.histogram[0].samples, "9007199254740993");
    assert_eq!(restored.distribution.origin, RuntimeDistributionOrigin::NativeHistogram);
}

#[test]
fn independent_native_populations_clocks_scopes_and_qualifiers_have_distinct_keys() {
    let point = sample();
    let key = point.key().unwrap();
    for field in ["source", "clock", "scope", "identity"] {
        let mut other = point.clone();
        match field {
            "source" => other.distribution.source = "other-native-histogram".into(),
            "clock" => other.observed_at = "2020-01-01T12:00:00.123457Z".into(),
            "scope" => other.scope = MeasurementScope::Server,
            "identity" => {
                other.identity.qualifiers.insert("schema".into(), "another-db".into());
            }
            _ => unreachable!(),
        }
        assert_ne!(other.key().unwrap(), key, "{field}");
    }
    let mut equivalent_clock = point.clone();
    equivalent_clock.observed_at = "2020-01-01T14:00:00.123456+02:00".into();
    assert_eq!(equivalent_clock.key().unwrap(), key);
}

#[test]
fn operational_wrapper_requires_the_exact_declared_native_source_and_population_key() {
    let point = sample();
    let mut row = OperationalObservation {
        source: point.distribution.source.clone(),
        key: point.key().unwrap(),
        fact: OperationalFact::RuntimeDistribution(Box::new(point)),
    };
    assert!(row.is_valid());
    row.source = "another-native-source".into();
    assert!(!row.is_valid());
    row.source = "native-histogram".into();
    row.key = "different-key".into();
    assert!(!row.is_valid());
}

#[test]
fn invalid_clocks_identities_and_populations_cannot_authorize_export() {
    for field in ["clock", "identity", "reset", "population", "bounds"] {
        let mut point = sample();
        match field {
            "clock" => point.observed_at = "native-local-clock".into(),
            "identity" => point.identity.id.clear(),
            "reset" => point.reset = ResetMarker::At("unknown".into()),
            "population" => {
                point.distribution.population =
                    crate::DistributionPopulation::NativeCounters { sample_count: Some("1".into()) }
            }
            "bounds" => point.distribution.histogram[0].upper = RuntimeDuration::Picoseconds { value: "0".into() },
            _ => unreachable!(),
        }
        assert!(!point.is_valid(), "{field}");
        assert!(point.key().is_err(), "{field}");
    }
}
