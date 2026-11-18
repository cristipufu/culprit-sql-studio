use super::*;
use crate::{MeasurementScope, ResourceSample, SourceState, source_id, telemetry::*};

fn measurement() -> ResourceMeasurement {
    ResourceMeasurement {
        data_io_accounting: DataIoAccounting::DataFiles,
        at: "2020-01-01T06:00:15Z".into(),
        replica_id: Some("private-native-resource-member".into()),
        replica_role: Some(NativeResourceReplicaRole::MssqlDbResourceStats(MssqlResourceReplicaRole::NamedReplica)),
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            unit: ResourceUnit::Percent,
            scope: MeasurementScope::Database,
            statistic: ResourceStatistic::Mean,
            period_ms: Some(15_000),
            limit_basis: ResourceLimitBasis::DatabaseLimit,
            value: Some(0.0),
        }],
    }
}
fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.resources = vec![ResourceSample::from_measurement(measurement())];
    snapshot
}

#[test]
fn sharing_masks_only_genuine_native_member_ids_and_preserves_role_dimensions_and_repeat_stability() {
    let original = snapshot();
    let shared = redact_snapshot(&original);
    let detail = shared.resources[0].measurement.as_ref().unwrap();
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-native-resource-member"));
    assert!(detail.replica_id.as_deref().unwrap().starts_with("redacted-"));
    assert_eq!(detail.replica_role, measurement().replica_role);
    assert_eq!(detail.metrics, measurement().metrics);
    assert!(shared.resources[0].has_consistent_measurement());
    let repeated = redact_snapshot(&shared);
    assert_eq!(repeated.resources[0].measurement, shared.resources[0].measurement);
    let mut unknown = original;
    unknown.resources[0].measurement.as_mut().unwrap().replica_id = None;
    let shared = redact_snapshot(&unknown);
    assert_eq!(shared.resources[0].measurement.as_ref().unwrap().replica_id, None);
}

#[test]
fn malformed_or_mismatched_original_details_cannot_be_repaired_by_masking_or_disappear_as_measured_empty() {
    for mutation in ["identity", "clock", "accounting", "chart"] {
        let mut original = snapshot();
        match mutation {
            "identity" => {
                original.resources[0].measurement.as_mut().unwrap().replica_id =
                    Some("private-native-resource-member".repeat(20))
            }
            "clock" => original.resources[0].measurement.as_mut().unwrap().at = "2020-01-01T06:00:30Z".into(),
            "accounting" => original.resources[0].data_io_accounting = DataIoAccounting::ComputeLocalSsd,
            "chart" => original.resources[0].cpu_pct = Some(99.0),
            _ => unreachable!(),
        }
        assert!(!original.resources[0].has_consistent_measurement());
        let shared = redact_snapshot(&original);
        assert!(shared.resources.is_empty());
        assert!(
            shared
                .sources
                .iter()
                .any(|source| source.source == source_id::RESOURCES && source.state == SourceState::Unavailable)
        );
        assert!(!serde_json::to_string(&shared).unwrap().contains("private-native-resource-member"));
    }
}

fn all_resource_dimensions() -> ResourceMeasurement {
    use MeasurementScope::{Database, Server};
    use ResourceLimitBasis::{Absolute, DatabaseLimit, InstanceLimit, PoolLimit};
    use ResourceStatistic::{Maximum, Mean, Point};
    use ResourceUnit::{Cores, DatabaseTransactionUnits, Percent};
    let mut native = measurement();
    native.metrics = [
        ("cpu", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(0.0)),
        ("data-io", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(20.0)),
        ("log-write", Database, Percent, Mean, Some(15_000), DatabaseLimit, Some(90.0)),
        ("memory", Database, Percent, Mean, Some(15_000), DatabaseLimit, None),
        ("workers", Database, Percent, Maximum, Some(15_000), DatabaseLimit, Some(55.0)),
        ("sessions", Database, Percent, Maximum, Some(15_000), DatabaseLimit, Some(30.0)),
        ("xtp-storage", Database, Percent, Point, None, PoolLimit, Some(0.0)),
        ("cpu", Server, Percent, Mean, Some(60_000), InstanceLimit, Some(70.0)),
        ("memory", Server, Percent, Mean, None, InstanceLimit, Some(40.0)),
        ("dtu-limit", Database, DatabaseTransactionUnits, Point, None, Absolute, None),
        ("cpu-limit", Database, Cores, Point, None, Absolute, Some(10.0)),
    ]
    .into_iter()
    .map(|(key, scope, unit, statistic, period_ms, limit_basis, value)| ResourceMetricSample {
        key: key.into(),
        scope,
        unit,
        statistic,
        period_ms,
        limit_basis,
        value,
    })
    .collect();
    native
}

#[test]
fn sharing_masks_valid_private_metric_keys_and_keeps_all_canonical_dimensions_and_numeric_semantics() {
    let mut native = all_resource_dimensions();
    let mut private = native.metrics[6].clone();
    private.key = "private_metric_secret_canary".into();
    private.unit = ResourceUnit::Bytes;
    private.limit_basis = ResourceLimitBasis::Absolute;
    private.value = Some(123.5);
    native.metrics.push(private.clone());
    assert!(native.is_valid(), "the canary is a valid original key, not malformed evidence");
    let mut original = snapshot();
    original.resources = vec![ResourceSample::from_measurement(native.clone())];
    assert!(original.resources[0].has_consistent_measurement());
    let shared = redact_snapshot(&original);
    let point = &shared.resources[0];
    let measurement = point.measurement.as_ref().unwrap();
    assert_eq!(measurement.metrics.len(), 12);
    assert_eq!(&measurement.metrics[..11], &native.metrics[..11]);
    assert_eq!(measurement.replica_role, native.replica_role);
    assert_eq!(measurement.at, native.at);
    assert_eq!(measurement.data_io_accounting, native.data_io_accounting);
    assert_eq!(
        [point.cpu_pct, point.data_io_pct, point.log_write_pct, point.memory_pct, point.workers_pct],
        [Some(0.0), Some(20.0), Some(90.0), None, Some(55.0)]
    );
    let expected_key = opaque(&private.key);
    private.key = expected_key.clone();
    assert_eq!(measurement.metrics[11], private);
    assert!(measurement.is_valid() && point.has_consistent_measurement());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private_metric_secret_canary"));
    assert!(serde_json::to_string(&shared).unwrap().contains(&expected_key));
    assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
    assert_eq!(original.resources[0].measurement.as_deref(), Some(&native), "sharing changes only the clone");
}

#[test]
fn private_metric_collisions_and_invalid_original_keys_leave_the_existing_explicit_unavailable_gap() {
    for case in ["collision", "invalid-original"] {
        let mut native = all_resource_dimensions();
        let mut private = native.metrics[6].clone();
        private.key = "private_metric_secret_canary".into();
        if case == "collision" {
            native.metrics.push(private.clone());
            private.key = opaque(&private.key);
            native.metrics.push(private);
            assert!(native.is_valid(), "distinct valid original keys collide only after masking");
        } else {
            private.key = "private_metric_secret_canary".repeat(10);
            native.metrics.push(private);
            assert!(!native.is_valid(), "an oversized original must not become valid after shortening");
        }
        let mut original = snapshot();
        original.resources = vec![ResourceSample::from_measurement(native)];
        assert_eq!(original.resources[0].has_consistent_measurement(), case == "collision");
        let shared = redact_snapshot(&original);
        assert!(shared.resources.is_empty(), "{case}: do not overwrite or retain a repaired partial row");
        let gap = shared.sources.iter().find(|source| source.source == source_id::RESOURCES).unwrap();
        assert_eq!(gap.state, SourceState::Unavailable);
        assert_eq!(gap.reason_message, Some(crate::message!("collection.invalid_source")));
        assert!(!serde_json::to_string(&shared).unwrap().contains("private_metric_secret_canary"));
        assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
    }
}
