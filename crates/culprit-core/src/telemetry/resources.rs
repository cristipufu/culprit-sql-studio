//! Provider-neutral resource samples. Native adapters supply explicit units, scope and aggregation.
use super::*;

/// What the native data-I/O utilization percentage accounts for. Its measurement scope and limit denominator
/// remain independent; an unknown deployment must not inherit conventional storage semantics.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DataIoAccounting {
    #[default]
    Unknown,
    /// Governed data-file I/O includes both reads and writes; it is independent of transaction-log throughput.
    DataFiles,
    /// Local SSD operations from the compute cache and temporary database, excluding remote page-server I/O.
    ComputeLocalSsd,
}
impl DataIoAccounting {
    /// A mixed population has no common accounting basis. The original samples remain independently available.
    pub fn common(values: impl IntoIterator<Item = Self>) -> Option<Self> {
        let mut values = values.into_iter();
        let first = values.next()?;
        values.all(|value| value == first).then_some(first)
    }

    pub fn label_message(self) -> crate::i18n::Message {
        match self {
            Self::Unknown => crate::message!("core.resource_io.unknown.label"),
            Self::DataFiles => crate::message!("core.resource_io.data_files.label"),
            Self::ComputeLocalSsd => crate::message!("core.resource_io.compute_local_ssd.label"),
        }
    }
    pub fn label(self) -> String {
        self.label_message().render()
    }

    pub fn meaning(self) -> String {
        match self {
            Self::Unknown => crate::t!("core.resource_io.unknown.meaning"),
            Self::DataFiles => crate::t!("core.resource_io.data_files.meaning"),
            Self::ComputeLocalSsd => crate::t!("core.resource_io.compute_local_ssd.meaning"),
        }
    }
}

/// A native role observation is independent from the identity of the member that supplied a historical row.
/// The source tag establishes the native vocabulary; no role supplies physical-member continuity.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "source", content = "role", rename_all = "kebab-case")]
#[ts(export)]
pub enum NativeResourceReplicaRole {
    MssqlDbResourceStats(MssqlResourceReplicaRole),
}

/// `sys.dm_db_resource_stats.replica_role` reports the source's current role, not a native replica ID or proof of
/// the role at an earlier sample timestamp. In particular, ReadOnly
/// application intent reports HighAvailabilitySecondary even when the endpoint is a geo-forwarder or named
/// replica; without that intent the native values can instead be GeoReplicationForwarder or NamedReplica.
/// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-resource-stats-azure-sql-database
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlResourceReplicaRole {
    Primary,
    HighAvailabilitySecondary,
    GeoReplicationForwarder,
    NamedReplica,
}
impl MssqlResourceReplicaRole {
    pub fn from_native_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Primary),
            1 => Some(Self::HighAvailabilitySecondary),
            2 => Some(Self::GeoReplicationForwarder),
            3 => Some(Self::NamedReplica),
            _ => None,
        }
    }
}

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
    pub data_io_accounting: DataIoAccounting,
    /// Native UTC timestamp. The collection window selects samples without prorating their values.
    pub at: String,
    /// Native replica identity only when the source binds this historical row to that member. A role code or
    /// the current reader's ReplicaID does not establish this relationship; a pool sample invents neither.
    pub replica_id: Option<String>,
    /// The source-reported role observed while reading this row, not proof of the role at its historical timestamp.
    /// Missing never supplies a primary, member identity or ownership.
    pub replica_role: Option<NativeResourceReplicaRole>,
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
            replica_role: None,
            data_io_accounting: crate::telemetry::DataIoAccounting::Unknown,
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
            partition: crate::telemetry::SourcePartition::Target,
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
    fn canonical_receipt_matching_preserves_exact_payload_bytes_and_native_scope() {
        let source = source();
        let capture = CaptureIdentity {
            observation_id: "canonical-resource-read".into(),
            revision: Some(CaptureRevision {
                outbox_id: "canonical-resource-owner".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let original = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(sample())).unwrap();
        let reference =
            DiagnosticEvidenceRef::from_export(&source, &digest(b"original manifest"), &original, 0).unwrap();
        reference.validate_canonical_record(&source, &original.record).unwrap();

        for value in [1.0, -0.0] {
            let mut changed = original.record.clone();
            let TelemetryRecord::ResourceSample(sample) = &mut changed else { panic!() };
            sample.metrics[0].value = Some(value);
            assert!(changed.is_valid());
            if value.is_sign_negative() {
                assert_eq!(changed, original.record, "float equality does not establish byte identity");
            }
            assert!(reference.validate_canonical_record(&source, &changed).is_err());
        }
        let mut changed = source.clone();
        changed.source = "another-native-resource-read".into();
        assert!(reference.validate_canonical_record(&changed, &original.record).is_err());
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
        record.replica_id = Some("native-member".into());
        let present = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(record.clone())).unwrap();
        assert_ne!(absent.record_id, present.record_id);
        record.replica_id = Some(String::new());
        assert!(!record.is_valid());
    }

    #[test]
    fn native_role_codes_preserve_source_vocabulary_without_creating_members() {
        use MssqlResourceReplicaRole::*;
        for (code, expected, text) in [
            (0, Primary, "primary"),
            (1, HighAvailabilitySecondary, "high-availability-secondary"),
            (2, GeoReplicationForwarder, "geo-replication-forwarder"),
            (3, NamedReplica, "named-replica"),
        ] {
            assert_eq!(MssqlResourceReplicaRole::from_native_code(code), Some(expected));
            let fact = NativeResourceReplicaRole::MssqlDbResourceStats(expected);
            let encoded = serde_json::to_value(fact).unwrap();
            assert_eq!(encoded, serde_json::json!({"source":"mssql-db-resource-stats","role":text}));
            assert_eq!(serde_json::from_value::<NativeResourceReplicaRole>(encoded).unwrap(), fact);
        }
        for code in [-1, 4, i64::MAX] {
            assert_eq!(MssqlResourceReplicaRole::from_native_code(code), None);
        }
        for encoded in [
            serde_json::json!({"source":"current-reader","role":"primary"}),
            serde_json::json!({"source":"mssql-db-resource-stats","role":0}),
            serde_json::json!({"source":"mssql-db-resource-stats","role":"secondary"}),
        ] {
            assert!(serde_json::from_value::<NativeResourceReplicaRole>(encoded).is_err());
        }
    }

    #[test]
    fn same_clock_role_changes_never_mint_new_member_identity_or_reuse_original_payload_proof() {
        let source = source();
        let capture = CaptureIdentity {
            observation_id: "role-facts".into(),
            revision: Some(CaptureRevision {
                outbox_id: "role-owner".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let mut native = sample();
        native.replica_id = None;
        native.replica_role = Some(NativeResourceReplicaRole::MssqlDbResourceStats(MssqlResourceReplicaRole::Primary));
        let original = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(native.clone())).unwrap();
        let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"native manifest"), &original, 0).unwrap();
        for role in
            [None, Some(NativeResourceReplicaRole::MssqlDbResourceStats(MssqlResourceReplicaRole::NamedReplica))]
        {
            native.replica_role = role;
            let changed =
                ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(native.clone())).unwrap();
            assert_eq!(changed.record_id, original.record_id, "a role is not a new native owner");
            assert_ne!(changed.record, original.record);
            assert!(reference.validate_canonical_record(&source, &changed.record).is_err());
            let TelemetryRecord::ResourceSample(changed) = changed.record else { panic!() };
            assert_eq!(changed.replica_id, None);
            assert_eq!(changed.replica_role, role);
        }
    }

    #[test]
    fn scan_projection_keeps_same_row_detail_and_never_substitutes_shared_or_unknown_values() {
        let mut native = sample();
        native.replica_id = None;
        native.replica_role =
            Some(NativeResourceReplicaRole::MssqlDbResourceStats(MssqlResourceReplicaRole::HighAvailabilitySecondary));
        let mut shared = native.metrics[0].clone();
        shared.scope = crate::MeasurementScope::Server;
        shared.value = Some(99.0);
        native.metrics.push(shared);
        native.metrics[0].value = None;
        let scan = crate::ResourceSample::from_measurement(native.clone());
        assert_eq!(scan.cpu_pct, None);
        assert_eq!(scan.measurement.as_deref(), Some(&native));
        assert!(scan.has_consistent_measurement());
        let encoded = serde_json::to_string(&scan).unwrap();
        let decoded: crate::ResourceSample = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.has_consistent_measurement());
        assert_eq!(decoded.measurement.as_deref(), Some(&native));
        for mutation in ["clock", "accounting", "chart", "invalid-detail"] {
            let mut changed = decoded.clone();
            match mutation {
                "clock" => changed.at = "2020-01-01T06:00:30Z".into(),
                "accounting" => changed.data_io_accounting = DataIoAccounting::ComputeLocalSsd,
                "chart" => changed.cpu_pct = Some(99.0),
                "invalid-detail" => changed.measurement.as_mut().unwrap().metrics.clear(),
                _ => unreachable!(),
            }
            assert!(!changed.has_consistent_measurement(), "{mutation}");
        }
        let mut missing = scan;
        missing.measurement = None;
        assert!(missing.has_consistent_measurement());
    }

    #[test]
    fn database_chart_percentages_never_borrow_a_different_capacity_denominator() {
        for basis in [ResourceLimitBasis::PoolLimit, ResourceLimitBasis::InstanceLimit, ResourceLimitBasis::Absolute] {
            let mut native = sample();
            native.metrics[0].limit_basis = basis;
            native.metrics[0].value = Some(90.0);
            assert!(native.metrics[0].describes_subject(&source().target));
            let scan = crate::ResourceSample::from_measurement(native.clone());
            assert_eq!(scan.cpu_pct, None, "the legacy chart cannot express {basis:?}");
            assert_eq!(scan.measurement.as_deref(), Some(&native));
            assert!(scan.has_consistent_measurement());
        }
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
    #[test]
    fn accounting_is_explicit_and_mixed_or_missing_populations_have_no_common_basis() {
        use DataIoAccounting::{ComputeLocalSsd, DataFiles, Unknown};
        assert_eq!(DataIoAccounting::common([]), None);
        assert_eq!(DataIoAccounting::common([Unknown, Unknown]), Some(Unknown));
        assert_eq!(DataIoAccounting::common([DataFiles, DataFiles]), Some(DataFiles));
        assert_eq!(DataIoAccounting::common([ComputeLocalSsd, ComputeLocalSsd]), Some(ComputeLocalSsd));
        assert_eq!(DataIoAccounting::common([DataFiles, ComputeLocalSsd]), None);
        assert_eq!(DataIoAccounting::common([Unknown, ComputeLocalSsd]), None);
        let mut encoded = serde_json::to_value(sample()).unwrap();
        encoded.as_object_mut().unwrap().remove("data_io_accounting");
        assert!(serde_json::from_value::<ResourceMeasurement>(encoded.clone()).is_err());
        encoded["data_io_accounting"] = serde_json::json!("Hyperscale");
        assert!(serde_json::from_value::<ResourceMeasurement>(encoded).is_err());
    }
}
