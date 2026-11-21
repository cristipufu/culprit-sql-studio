use super::*;
use crate::{Engine, MeasurementScope, SourceState};

fn layout() -> MssqlManagedInstanceResourceLayout {
    let value = MssqlCatalogAvailability::Available;
    MssqlManagedInstanceResourceLayout {
        interval: value,
        cpu: value,
        virtual_core_count: value,
        reserved_storage_mb: value,
        storage_space_used_mb: value,
        io_request: value,
        io_bytes_read: value,
        io_bytes_written: value,
        resource_type: value,
        resource_name: value,
        sku: value,
        hardware_generation: value,
    }
}
fn measurement() -> ResourceMeasurement {
    ResourceMeasurement {
        at: "2020-01-01T06:00:15.000000000Z".into(),
        data_io_accounting: DataIoAccounting::Unknown,
        replica_id: None,
        replica_role: None,
        metrics: vec![ResourceMetricSample {
            key: "cpu".into(),
            scope: MeasurementScope::Server,
            unit: ResourceUnit::Percent,
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
            reserved_storage_mb: Some("9223372036854775807".into()),
            storage_space_used_mb: Some("9007199254740992.25".into()),
            io_request: Some("9007199254740993".into()),
            io_bytes_read: Some("9223372036854775807".into()),
            io_bytes_written: Some("0".into()),
            resource_type: Some("managed_instance".into()),
            resource_name: Some("private-mi".into()),
            sku: Some("General Purpose".into()),
            hardware_generation: None,
        })),
    }
}
fn source() -> CollectionDescription {
    let owner = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("reader-machine"),
        native: NativeIdentity::new("reader-instance"),
    };
    let target = CollectionTarget::Shared(owner.clone());
    CollectionDescription {
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
                    epoch: Some("current-reader-process".into()),
                },
                observed_from: "2020-01-01T06:10:00Z".into(),
                observed_until: "2020-01-01T06:10:00Z".into(),
                database_writable: None,
            })),
            coverage: StateSourceCoverage::collected(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 1),
            native_rows: 1,
            layout: layout(),
        })),
    }
}
fn header(source: &mut CollectionDescription) -> &mut MssqlManagedInstanceResourceHistory {
    let SourceObservation::MssqlManagedInstanceResources(history) = &mut source.observation else { panic!() };
    history
}
#[test]
fn native_interval_totals_and_wide_storage_decimals_are_distinct_from_the_cpu_mean() {
    let row = measurement();
    assert!(row.is_valid());
    let native = row.mssql_managed_instance.as_ref().unwrap();
    assert_eq!(native.io_request.as_deref(), Some("9007199254740993"));
    assert_eq!(native.storage_space_used_mb.as_deref(), Some("9007199254740992.25"));
    assert_eq!(row.metrics.len(), 1);
    assert_eq!(row.metrics[0].statistic, ResourceStatistic::Mean);
    let roundtrip: ResourceMeasurement = serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
    assert_eq!(roundtrip, row);
    for malformed in ["-1", "01", "1.0", "9223372036854775808"] {
        let mut changed = row.clone();
        changed.mssql_managed_instance.as_mut().unwrap().io_request = Some(malformed.into());
        assert!(!changed.is_valid(), "{malformed}");
    }
    for malformed in ["12", "12.3", "012.34", "NaN", "-0.01", "1000.00"] {
        let mut changed = row.clone();
        changed.mssql_managed_instance.as_mut().unwrap().avg_cpu_percent = Some(malformed.into());
        assert!(!changed.is_valid(), "{malformed}");
    }
}
#[test]
fn optional_layout_and_native_null_do_not_supply_an_observed_zero_or_a_denominator() {
    let mut row = measurement();
    let mut native_layout = layout();
    {
        let native = row.mssql_managed_instance.as_mut().unwrap();
        native.avg_cpu_percent = None;
        native.virtual_core_count = None;
        native.io_bytes_written = None;
    }
    row.metrics[0].value = None;
    native_layout.io_bytes_written = MssqlCatalogAvailability::Unsupported;
    assert!(row.is_valid());
    assert!(row.mssql_managed_instance.as_ref().unwrap().fits_layout(&native_layout));
    row.mssql_managed_instance.as_mut().unwrap().io_bytes_written = Some("0".into());
    assert!(!row.mssql_managed_instance.as_ref().unwrap().fits_layout(&native_layout));
    native_layout.io_bytes_written = MssqlCatalogAvailability::Available;
    assert!(row.mssql_managed_instance.as_ref().unwrap().fits_layout(&native_layout));
    row.metrics[0].value = Some(0.0);
    assert!(!row.is_valid(), "NULL CPU cannot become measured zero");
}
#[test]
fn historical_intervals_never_inherit_current_observer_member_role_or_process() {
    let source = source();
    assert!(source.partition_is_consistent());
    let SourceObservation::MssqlManagedInstanceResources(history) = &source.observation else { panic!() };
    let row = measurement();
    assert!(history.accepts(&row));
    assert_ne!(history.current_reader.as_ref().unwrap().observed_from, row.at);
    for changed in ["replica", "role", "scope", "basis", "period", "clock", "interval"] {
        let mut row = row.clone();
        match changed {
            "replica" => row.replica_id = Some("current-reader-instance".into()),
            "role" => {
                row.replica_role =
                    Some(NativeResourceReplicaRole::MssqlDbResourceStats(MssqlResourceReplicaRole::Primary))
            }
            "scope" => row.metrics[0].scope = MeasurementScope::Database,
            "basis" => row.metrics[0].limit_basis = ResourceLimitBasis::DatabaseLimit,
            "period" => row.metrics[0].period_ms = Some(60_000),
            "clock" => row.at = "2020-01-01T06:00:30.000000000Z".into(),
            "interval" => row.mssql_managed_instance.as_mut().unwrap().started_at = row.at.clone(),
            _ => unreachable!(),
        };
        assert!(!history.accepts(&row), "{changed}");
    }
}
#[test]
fn only_the_original_known_mi_reader_can_bind_the_selected_history_source() {
    for mutation in ["engine", "target", "member", "epoch", "clock", "edition", "partition", "source"] {
        let mut changed = source();
        match mutation {
            "engine" => {
                let CollectionTarget::Shared(owner) = &mut changed.target else { panic!() };
                owner.engine = Engine::Postgres;
            }
            "target" => {
                let CollectionTarget::Shared(owner) = &mut changed.target else { panic!() };
                owner.native = NativeIdentity::new("other-instance");
            }
            "member" => {
                let SourcePartition::InstanceLocal { member, .. } =
                    &mut header(&mut changed).current_reader.as_mut().unwrap().partition
                else {
                    panic!()
                };
                member.as_mut().unwrap().native = NativeIdentity::new("other-member");
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } =
                    &mut header(&mut changed).current_reader.as_mut().unwrap().partition
                else {
                    panic!()
                };
                *epoch = None;
            }
            "clock" => {
                header(&mut changed).current_reader.as_mut().unwrap().observed_until = "2020-01-01T06:09:58Z".into()
            }
            "edition" => header(&mut changed).native_engine_edition = 3,
            "partition" => changed.partition = header(&mut changed).current_reader.as_ref().unwrap().partition.clone(),
            "source" => changed.source = crate::source_id::RESOURCES.into(),
            _ => unreachable!(),
        };
        assert!(!changed.partition_is_consistent(), "{mutation}");
        let capture = CaptureIdentity { observation_id: "mi-original".into(), revision: None };
        assert!(
            ExportedRecord::new(&changed, &capture, TelemetryRecord::ResourceSample(measurement())).is_err(),
            "{mutation}"
        );
    }
}
#[test]
fn prefix_lookahead_and_delayed_empty_history_remain_explicitly_partial() {
    let mut source = source();
    let history = header(&mut source);
    history.coverage = StateSourceCoverage::observed(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 1, false);
    history.coverage.stopped = Some(CollectionStop::RowLimit);
    history.native_rows = 2;
    assert!(history.is_valid());
    assert!(history.accepts(&measurement()));
    history.native_rows = 1;
    assert!(!history.is_valid());
    history.coverage = StateSourceCoverage::observed(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 0, false);
    history.native_rows = 0;
    assert!(history.is_valid());
    assert_eq!(history.coverage.state, SourceState::Partial);
    assert!(!history.coverage.complete_scope);
    history.coverage = StateSourceCoverage::collected(MSSQL_MANAGED_INSTANCE_RESOURCES_SOURCE.into(), 0);
    assert!(!history.is_valid(), "no delayed source row proves quiet health");
}
#[test]
fn original_receipts_preserve_full_native_rows_and_withdraw_mutated_interval_facts() {
    let source = source();
    let capture = CaptureIdentity {
        observation_id: "mi-original".into(),
        revision: Some(CaptureRevision {
            outbox_id: "mi-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let original = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(measurement())).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&source, &digest(b"original-mi-manifest"), &original, 0).unwrap();
    reference.validate_canonical_record(&source, &original.record).unwrap();
    let mut changed = original.record.clone();
    let TelemetryRecord::ResourceSample(row) = &mut changed else { panic!() };
    row.mssql_managed_instance.as_mut().unwrap().io_bytes_written = Some("1".into());
    assert!(reference.validate_canonical_record(&source, &changed).is_err());
    let mut unrelated = source.clone();
    unrelated.observation = SourceObservation::RetainedSamples(SampleHistoryDescription {
        from: "2020-01-01T06:00:00Z".into(),
        until: "2020-01-01T06:01:00Z".into(),
        observed_at: "2020-01-01T06:10:00Z".into(),
        available_from: Some("2020-01-01T05:00:00Z".into()),
        available_until: Some("2020-01-01T06:01:00Z".into()),
        expected_samples: 1,
        nominal_period_ms: Some(15_000),
    });
    assert!(
        ExportedRecord::new(&unrelated, &capture, original.record).is_err(),
        "the ordinary resource header cannot adopt MI interval facts"
    );
}

fn captured_read() -> CapturedManagedInstanceResourceRead {
    let source = source();
    let capture = CaptureIdentity {
        observation_id: "mi-saved-original".into(),
        revision: Some(CaptureRevision {
            outbox_id: "mi-saved-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let row = ExportedRecord::new(&source, &capture, TelemetryRecord::ResourceSample(measurement())).unwrap();
    CapturedManagedInstanceResourceRead::captured(
        source.clone(),
        DiagnosticCaptureRef {
            capture,
            manifest_sha256: digest(b"mi saved manifest"),
            source_scope_id: source.captured_range().unwrap().scope_id,
        },
        vec![CapturedSnapshotPosition::from_export(&row, 0).unwrap()],
        vec![measurement()],
        true,
    )
    .unwrap()
}
#[test]
fn saved_history_uses_exact_original_receipts_for_native_values_scope_counts_and_positions() {
    let original = captured_read();
    assert!(original.is_valid());
    let encoded = serde_json::to_value(&original).unwrap();
    let restored: CapturedManagedInstanceResourceRead = serde_json::from_value(encoded).unwrap();
    assert!(restored.is_valid());
    assert_eq!(restored, original);
    for label in ["value", "scope", "count", "ordinal", "digest"] {
        let mut changed = original.clone();
        match label {
            "value" => changed.observations[0].mssql_managed_instance.as_mut().unwrap().io_request = Some("1".into()),
            "scope" => changed.description.source = "resources".into(),
            "count" => {
                let SourceObservation::MssqlManagedInstanceResources(history) = &mut changed.description.observation
                else {
                    panic!()
                };
                history.coverage.records = 2;
                history.native_rows = 2;
            }
            "ordinal" => changed.origin.positions[0].row_ordinal = 1,
            "digest" => changed.origin.positions[0].payload_sha256 = digest(b"altered"),
            _ => unreachable!(),
        };
        assert!(!changed.is_valid(), "{label}");
    }
}
#[test]
fn partial_body_keeps_original_metadata_and_no_unauthenticated_native_values() {
    let mut read = captured_read();
    read.observations.clear();
    read.origin.positions.clear();
    read.read_complete = false;
    assert!(read.is_valid());
    let SourceObservation::MssqlManagedInstanceResources(history) = &read.description.observation else { panic!() };
    assert_eq!(history.coverage.records, 1);
    read.read_complete = true;
    assert!(!read.is_valid());
}
