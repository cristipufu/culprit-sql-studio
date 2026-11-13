//! Provider-neutral resource samples. Native adapters supply explicit units, scope and aggregation.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ResourceUnit {
    Percent,
    Cores,
    Bytes,
    DatabaseTransactionUnits,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ResourceStatistic {
    Mean,
    Maximum,
    Point,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ResourceLimitBasis {
    DatabaseLimit,
    InstanceLimit,
    PoolLimit,
    Absolute,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ResourceMetricSample {
    /// Stable machine identifier, independent of translated labels or the destination schema.
    pub key: String,
    pub unit: ResourceUnit,
    pub scope: crate::MeasurementScope,
    pub statistic: ResourceStatistic,
    /// None when the native source does not establish an aggregation period.
    pub period_ms: Option<u32>,
    pub limit_basis: ResourceLimitBasis,
    /// A native NULL remains unknown, never a measured zero.
    pub value: Option<f64>,
}
impl ResourceMetricSample {
    /// A capacity denominator does not change which resource was measured. Broader or unresolved
    /// context can explain a symptom but cannot be charged to the subject of this collection.
    pub fn describes_subject(&self, target: &CollectionTarget) -> bool {
        self.scope == target.measurement_scope()
    }

    fn is_valid(&self) -> bool {
        !self.key.is_empty()
            && self.key.len() <= 128
            && self
                .key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.'))
            && self.period_ms != Some(0)
            && self.value.is_none_or(|value| value.is_finite() && value >= 0.0)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ResourceMeasurement {
    /// Native UTC timestamp. The collection window selects samples without prorating their values.
    pub at: String,
    /// Native replica identity when supplied by the source. A pool sample does not invent a replica.
    pub replica_id: Option<String>,
    pub metrics: Vec<ResourceMetricSample>,
}
impl ResourceMeasurement {
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::HashSet::new();
        self.at.len() <= 64
            && DateTime::parse_from_rfc3339(&self.at).is_ok()
            && self
                .replica_id
                .as_deref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 128 && id.chars().all(|ch| !ch.is_control()))
            && (1..=64).contains(&self.metrics.len())
            && self
                .metrics
                .iter()
                .all(|metric| metric.is_valid() && keys.insert((metric.key.as_str(), metric.scope, metric.statistic)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> ResourceMeasurement {
        ResourceMeasurement {
            at: "2020-01-01T06:00:15Z".into(),
            replica_id: Some("primary".into()),
            metrics: vec![ResourceMetricSample {
                key: "cpu".into(),
                scope: crate::MeasurementScope::Database,
                unit: ResourceUnit::Percent,
                statistic: ResourceStatistic::Mean,
                period_ms: Some(15_000),
                limit_basis: ResourceLimitBasis::DatabaseLimit,
                value: Some(0.0),
            }],
        }
    }
    fn source() -> CollectionDescription {
        CollectionDescription {
            target: crate::DatabaseIdentity::server(crate::Engine::Postgres, "fixture", "fixture.invalid", 5432, None)
                .into(),
            source: "native-resources".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::RetainedSamples(SampleHistoryDescription {
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T06:01:00Z".into(),
                observed_at: "2020-01-01T06:02:00Z".into(),
                available_from: Some("2020-01-01T05:55:00Z".into()),
                available_until: Some("2020-01-01T06:01:30Z".into()),
                expected_samples: 1,
                nominal_period_ms: Some(15_000),
            }),
        }
    }
    #[test]
    fn overlapping_windows_and_timestamp_offsets_preserve_resource_measurement_identity() {
        let source = source();
        let capture = CaptureIdentity { observation_id: "first".into(), revision: None };
        let original = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(sample())).unwrap();
        let mut overlap = source.clone();
        let SourceObservation::RetainedSamples(history) = &mut overlap.observation else { unreachable!() };
        history.from = "2020-01-01T05:59:00Z".into();
        let mut row = sample();
        row.at = "2020-01-01T08:00:15+02:00".into();
        let repeated = ExportedRecord::new(
            &overlap,
            &CaptureIdentity { observation_id: "second".into(), revision: None },
            TelemetryRecord::ResourceSample(row),
        )
        .unwrap();
        assert_eq!(original.record_id, repeated.record_id);
        assert_ne!(original.batch_id, repeated.batch_id);
        let mut row = sample();
        row.at = "2020-01-01T06:01:00Z".into();
        assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(row)).is_err());
    }

    #[test]
    fn absent_replica_and_unpromised_period_remain_unknown_instead_of_a_primary_or_default_interval() {
        let mut source = source();
        let SourceObservation::RetainedSamples(history) = &mut source.observation else { unreachable!() };
        history.nominal_period_ms = None;
        let capture = CaptureIdentity { observation_id: "unknown-replica".into(), revision: None };
        let mut record = sample();
        record.replica_id = None;
        assert!(record.is_valid());
        let absent = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(record.clone())).unwrap();
        record.replica_id = Some("0".into());
        let primary = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(record.clone())).unwrap();
        assert_ne!(absent.record_id, primary.record_id);
        record.replica_id = Some(String::new());
        assert!(!record.is_valid());
    }

    #[test]
    fn native_sample_availability_distinguishes_delayed_head_from_proven_lost_history() {
        let SourceObservation::RetainedSamples(mut history) = source().observation else { unreachable!() };
        history.nominal_period_ms = None;
        assert_eq!(history.coverage_stop(), None);
        history.available_until = Some("2020-01-01T06:00:30Z".into());
        assert_eq!(history.coverage_stop(), Some(CollectionStop::SourceNotReady));
        history.available_from = Some("2020-01-01T06:00:15Z".into());
        assert_eq!(history.coverage_stop(), Some(CollectionStop::RetentionGap));
        history.available_from = None;
        history.available_until = None;
        history.expected_samples = 0;
        assert_eq!(history.coverage_stop(), Some(CollectionStop::SourceNotReady));
        history.nominal_period_ms = Some(0);
        assert_eq!(history.coverage_stop(), Some(CollectionStop::InvalidPage));
    }
    #[test]
    fn resources_distinguish_shared_scope_and_unknown_values_and_reject_invalid_metrics() {
        let mut row = sample();
        row.metrics[0].value = None;
        assert!(row.is_valid());
        let mut shared = row.metrics[0].clone();
        shared.scope = crate::MeasurementScope::Server;
        row.metrics.push(shared);
        assert!(row.is_valid());
        row.metrics.push(row.metrics[0].clone());
        assert!(!row.is_valid());
        row.metrics.pop();
        row.metrics[0].value = Some(f64::NAN);
        assert!(!row.is_valid());
        row.metrics[0].value = Some(-1.0);
        assert!(!row.is_valid());
    }

    #[test]
    fn measured_subject_is_independent_from_its_limit_denominator_across_every_engine() {
        for engine in crate::Engine::ALL {
            let database =
                CollectionTarget::Database(DatabaseIdentity::server(engine, "db", "fixture.invalid", 1, None));
            let pool = CollectionTarget::Shared(SharedResourceIdentity {
                engine,
                kind: SharedResourceKind::Pool,
                namespace: NativeIdentity::new("instance"),
                native: NativeIdentity::new("pool"),
            });
            let mut metric = sample().metrics.remove(0);
            metric.limit_basis = ResourceLimitBasis::PoolLimit;
            assert!(metric.describes_subject(&database));
            assert!(!metric.describes_subject(&pool));
            metric.scope = crate::MeasurementScope::Pool;
            assert!(metric.describes_subject(&pool));
            assert!(!metric.describes_subject(&database));
            metric.scope = crate::MeasurementScope::Unknown;
            assert!(!metric.describes_subject(&pool));
            assert!(!metric.describes_subject(&database));
        }
    }
    #[test]
    fn retention_bounds_do_not_turn_expired_history_into_complete_emptiness() {
        let mut source = source();
        assert!(source.observation.covers_request());
        let SourceObservation::RetainedSamples(history) = &mut source.observation else { unreachable!() };
        history.available_from = Some("2020-01-01T06:00:15Z".into());
        assert!(!history.covers_request());
        history.available_from = None;
        history.available_until = None;
        history.expected_samples = 0;
        assert!(!history.covers_request());
    }
}
