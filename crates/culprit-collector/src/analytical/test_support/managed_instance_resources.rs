//! Controlled original MI history; this corpus executes no SQL and claims no live platform or cost proof.
use super::*;

pub fn managed_instance_resource_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let available = MssqlCatalogAvailability::Available;
    let owner = SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-current-machine"),
        native: NativeIdentity::new("private-current-instance"),
    };
    let target = CollectionTarget::Shared(owner.clone());
    let source = CollectionDescription {
        target: target.clone(),
        source: MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MssqlManagedInstanceResources(Box::new(MssqlManagedInstanceResourceHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
            native_engine_edition: 8,
            observed_at: "2020-01-01T06:10:00Z".into(),
            current_reader: Some(Box::new(NativeReadContext {
                target,
                partition: SourcePartition::InstanceLocal {
                    member: Some(owner),
                    epoch: Some("private-current-process".into()),
                },
                observed_from: "2020-01-01T06:10:00Z".into(),
                observed_until: "2020-01-01T06:10:00Z".into(),
                database_writable: None,
            })),
            coverage: StateSourceCoverage::collected(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 1),
            native_rows: 1,
            layout: MssqlManagedInstanceResourceLayout {
                interval: available,
                cpu: available,
                virtual_core_count: available,
                reserved_storage_mb: available,
                storage_space_used_mb: available,
                io_request: available,
                io_bytes_read: available,
                io_bytes_written: available,
                resource_type: available,
                resource_name: available,
                sku: available,
                hardware_generation: MssqlCatalogAvailability::Unsupported,
            },
        })),
    };
    let row = TelemetryRecord::ResourceSample(ResourceMeasurement {
        at: "2020-01-01T06:00:15.000000000Z".into(),
        data_io_accounting: DataIoAccounting::Unknown,
        replica_id: None,
        replica_role: None,
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            unit: ResourceUnit::Percent,
            scope: culprit_core::MeasurementScope::Server,
            statistic: ResourceStatistic::Mean,
            period_ms: Some(15_000),
            limit_basis: ResourceLimitBasis::InstanceLimit,
            value: Some(12.34),
        }],
        mssql_managed_instance: Some(Box::new(MssqlManagedInstanceResourceSample {
            started_at: "2020-01-01T06:00:00.000000000Z".into(),
            ended_at: "2020-01-01T06:00:15.000000000Z".into(),
            avg_cpu_percent: Some("12.34".into()),
            virtual_core_count: Some("8".into()),
            reserved_storage_mb: Some("1048576".into()),
            storage_space_used_mb: Some("9007199254740992.25".into()),
            io_request: Some("9007199254740993".into()),
            io_bytes_read: Some("9223372036854775807".into()),
            io_bytes_written: Some("0".into()),
            resource_type: Some("managed_instance".into()),
            resource_name: Some("private-mi-service-name".into()),
            sku: Some("General Purpose".into()),
            hardware_generation: None,
        })),
    });
    assert!(source.partition_is_consistent());
    ExportedRecord::new(&source, &fixture().0.capture, row.clone()).unwrap();
    (source, vec![row])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedInstanceResourceCase {
    Complete,
    Partial,
    Denied,
    Unsupported,
    NoRows,
    NativeNull,
    CpuUnsupported,
}
impl ManagedInstanceResourceCase {
    pub const ALL: [Self; 7] = [
        Self::Complete,
        Self::Partial,
        Self::Denied,
        Self::Unsupported,
        Self::NoRows,
        Self::NativeNull,
        Self::CpuUnsupported,
    ];
}
pub fn managed_instance_resource_case(
    case: ManagedInstanceResourceCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = managed_instance_resource_evidence();
    let SourceObservation::MssqlManagedInstanceResources(history) = &mut source.observation else { panic!() };
    match case {
        ManagedInstanceResourceCase::Complete => {}
        ManagedInstanceResourceCase::Partial => {
            history.coverage = StateSourceCoverage::observed(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 1, false);
            history.coverage.stopped = Some(CollectionStop::RowLimit);
            history.native_rows = 2;
        }
        ManagedInstanceResourceCase::Denied | ManagedInstanceResourceCase::Unsupported => {
            let denied = case == ManagedInstanceResourceCase::Denied;
            history.coverage = StateSourceCoverage::unavailable(
                MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(),
                if denied { culprit_core::ErrorCode::PermissionDenied } else { culprit_core::ErrorCode::Unsupported },
                None,
            );
            history.native_rows = 0;
            history.current_reader = None;
            history.layout.interval =
                if denied { MssqlCatalogAvailability::PermissionDenied } else { MssqlCatalogAvailability::Unsupported };
            rows.clear();
        }
        ManagedInstanceResourceCase::NoRows => {
            history.coverage = StateSourceCoverage::observed(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 0, false);
            history.native_rows = 0;
            rows.clear();
        }
        ManagedInstanceResourceCase::NativeNull | ManagedInstanceResourceCase::CpuUnsupported => {
            let TelemetryRecord::ResourceSample(row) = &mut rows[0] else { panic!() };
            row.metrics[0].value = None;
            let native = row.mssql_managed_instance.as_mut().unwrap();
            native.avg_cpu_percent = None;
            if case == ManagedInstanceResourceCase::CpuUnsupported {
                history.layout.cpu = MssqlCatalogAvailability::Unsupported;
            } else {
                native.virtual_core_count = None;
                native.storage_space_used_mb = None;
                native.io_request = None;
            }
        }
    }
    assert!(source.partition_is_consistent());
    for row in &rows {
        ExportedRecord::new(&source, &fixture().0.capture, row.clone()).unwrap();
    }
    (source, rows)
}
