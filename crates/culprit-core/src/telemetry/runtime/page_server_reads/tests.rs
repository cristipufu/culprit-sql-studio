use super::*;

fn statistics() -> PageServerReadStatistics {
    PageServerReadStatistics {
        availability: PageServerReadAvailability::Available,
        average_pages: Some(12.5),
        last_pages: Some("9007199254740993".into()),
        minimum_pages: Some("0".into()),
        maximum_pages: Some(i64::MAX.to_string()),
        standard_deviation_pages: Some(1.25),
    }
}

#[test]
fn native_float_and_exact_bigint_statistics_survive_without_becoming_elapsed_or_inclusive_totals() {
    let value = statistics();
    assert!(value.is_valid());
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(encoded["last_pages"], "9007199254740993");
    assert_eq!(encoded["maximum_pages"], i64::MAX.to_string());
    assert_eq!(encoded["average_pages"], 12.5);
    assert_eq!(serde_json::from_value::<PageServerReadStatistics>(encoded).unwrap(), value);
}

#[test]
fn available_native_null_and_zero_remain_distinct_from_unknown_and_not_applicable() {
    let mut value =
        PageServerReadStatistics { availability: PageServerReadAvailability::Available, ..Default::default() };
    assert!(value.is_valid() && !value.has_values());
    value.average_pages = Some(0.0);
    value.last_pages = Some("0".into());
    assert!(value.is_valid() && value.has_values());
    for availability in [
        PageServerReadAvailability::Unknown,
        PageServerReadAvailability::NotApplicable,
        PageServerReadAvailability::Unavailable,
    ] {
        value.availability = availability;
        assert!(!value.is_valid(), "sentinel values cannot become measured zero");
        value.average_pages = None;
        value.last_pages = None;
        assert!(value.is_valid());
        assert_eq!(
            PageServerReadAvailability::from_native_property(availability.native_property()),
            Some(availability)
        );
        value.last_pages = Some("0".into());
    }
}

#[test]
fn malformed_integer_or_float_statistics_do_not_become_unknown_or_zero() {
    for native in ["", "-1", "00", "+1", "1.0", "1e3", "9223372036854775808"] {
        let mut value = statistics();
        value.last_pages = Some(native.into());
        assert!(!value.is_valid(), "{native}");
    }
    for native in [-0.5, f64::NAN, f64::INFINITY] {
        let mut value = statistics();
        value.average_pages = Some(native);
        assert!(!value.is_valid());
    }
}

#[test]
fn derived_plan_statistics_cannot_choose_a_last_or_pool_native_standard_deviations() {
    let mut values = statistics();
    let mut summary = PageServerReadSummary::Derived {
        native_fragments: "2".into(),
        executions: "9007199254740993".into(),
        execution_type: Some("0".into()),
        statistics: Box::new(values.clone()),
    };
    assert!(!summary.is_valid());
    values.last_pages = None;
    values.standard_deviation_pages = None;
    summary = PageServerReadSummary::Derived {
        native_fragments: "2".into(),
        executions: "9007199254740993".into(),
        execution_type: Some("0".into()),
        statistics: Box::new(values.clone()),
    };
    assert!(summary.is_valid());
    for native_fragments in ["0", "1", "02", "18446744073709551615"] {
        assert!(
            !PageServerReadSummary::Derived {
                native_fragments: native_fragments.into(),
                executions: "9007199254740993".into(),
                execution_type: Some("0".into()),
                statistics: Box::new(values.clone())
            }
            .is_valid()
        );
    }
    assert!(
        PageServerReadSummary::NativeFragment {
            native: Some(NativeIdentity::new("17")),
            executions: "9007199254740993".into(),
            statistics: Box::new(statistics())
        }
        .is_valid()
    );
    assert!(
        !PageServerReadSummary::Derived {
            native_fragments: "2".into(),
            executions: "9007199254740993".into(),
            execution_type: None,
            statistics: Box::new(values)
        }
        .is_valid()
    );
}

fn original_runtime() -> (CollectionDescription, RuntimeMeasurement) {
    let source = CollectionDescription {
        target: crate::DatabaseIdentity {
            engine: crate::Engine::Mssql,
            location: crate::DatabaseLocation::Server { host: "fixture.invalid".into(), port: 1433, instance: None },
            database: "app".into(),
            incarnation: Some("native-logical-database".into()),
        }
        .into(),
        partition: SourcePartition::Target,
        source: "native-query-store".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            acquisition_owner: None,
            interval: TelemetryInterval {
                native_id: "1".into(),
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: [(
                PageServerReadAvailability::PROPERTY.into(),
                PageServerReadAvailability::Available.native_property().into(),
            )]
            .into(),
        }),
    };
    let row = RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity::new("17"),
            references: StatementReferences::default(),
            cohort: None,
        },
        executions: "1".into(),
        metrics: vec![],
        page_server_reads: Some(Box::new(statistics())),
        first_execution_at: None,
        last_execution_at: None,
    };
    (source, row)
}

#[test]
fn recorded_availability_must_match_the_original_header_and_cannot_be_erased_or_borrowed() {
    let (mut source, mut row) = original_runtime();
    assert!(row.page_server_reads_match_source(&source));
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    history.native_properties.insert(
        PageServerReadAvailability::PROPERTY.into(),
        PageServerReadAvailability::NotApplicable.native_property().into(),
    );
    assert!(!row.page_server_reads_match_source(&source));
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    history.native_properties.clear();
    assert!(!row.page_server_reads_match_source(&source));
    row.page_server_reads = None;
    assert!(row.page_server_reads_match_source(&source));
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    history.native_properties.insert(
        PageServerReadAvailability::PROPERTY.into(),
        PageServerReadAvailability::Unknown.native_property().into(),
    );
    assert!(!row.page_server_reads_match_source(&source));
}

fn capability(source: &mut CollectionDescription, value: Option<PageServerReadAvailability>) {
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    if let Some(value) = value {
        history.native_properties.insert(PageServerReadAvailability::PROPERTY.into(), value.native_property().into());
    } else {
        history.native_properties.remove(PageServerReadAvailability::PROPERTY);
    }
}
fn capture() -> CaptureIdentity {
    CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "1".into(), sequence: "1".into() }),
    }
}

#[test]
fn canonical_export_rejects_known_conflicting_capabilities_before_any_receipt_can_be_issued() {
    let (source, row) = original_runtime();
    let capture = capture();
    let record = TelemetryRecord::QueryRuntime(row);
    let exported = ExportedRecord::new(&source, &capture, record.clone()).unwrap();
    assert_eq!(exported.record, record);
    for refusal in [PageServerReadAvailability::NotApplicable, PageServerReadAvailability::Unavailable] {
        let mut contradictory = source.clone();
        capability(&mut contradictory, Some(refusal));
        assert_eq!(
            contradictory.captured_range().unwrap().scope_id,
            source.captured_range().unwrap().scope_id,
            "the refusal is capability agreement, not an unrelated scope or native-key hash change"
        );
        let parent = CapturedReadRef::new(
            &contradictory,
            capture.clone(),
            digest(b"same-original-manifest"),
            vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: 1 }],
        )
        .unwrap();
        assert!(parent.bind(&contradictory).is_ok(), "the supplied source bytes and original row count agree");
        assert!(ExportedRecord::new(&contradictory, &capture, record.clone()).is_err());
        assert!(
            CapturedRecordReference::from_export(&parent, &contradictory, &exported, 0).is_err(),
            "an informational position cannot bypass the canonical source/body contract"
        );
    }
}

#[test]
fn canonical_export_preserves_native_null_zero_missing_and_unknown_as_distinct_original_states() {
    let (mut source, mut row) = original_runtime();
    let capture = capture();
    row.page_server_reads = Some(Box::new(PageServerReadStatistics {
        availability: PageServerReadAvailability::Available,
        ..Default::default()
    }));
    let native_null = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
    row.page_server_reads.as_mut().unwrap().last_pages = Some("0".into());
    let zero = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
    assert_ne!(native_null.payload_sha256().unwrap(), zero.payload_sha256().unwrap());
    assert!(
        matches!(&native_null.record, TelemetryRecord::QueryRuntime(row) if row.page_server_reads.as_ref().is_some_and(|statistics| !statistics.has_values()))
    );
    assert!(
        matches!(&zero.record, TelemetryRecord::QueryRuntime(row) if row.page_server_reads.as_ref().unwrap().last_pages.as_deref()==Some("0"))
    );

    capability(&mut source, Some(PageServerReadAvailability::Unknown));
    row.page_server_reads = Some(Box::new(PageServerReadStatistics::default()));
    let unknown = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
    assert!(
        matches!(&unknown.record, TelemetryRecord::QueryRuntime(row) if row.page_server_reads.as_ref().unwrap().availability==PageServerReadAvailability::Unknown)
    );
    capability(&mut source, Some(PageServerReadAvailability::Available));
    assert!(
        ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).is_err(),
        "known source and Unknown row are not silently repaired into native-null Available"
    );
    row.page_server_reads = None;
    assert!(
        ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).is_err(),
        "a positively declared capability cannot erase its recorded field"
    );
    capability(&mut source, None);
    let missing = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
    assert!(matches!(&missing.record, TelemetryRecord::QueryRuntime(row) if row.page_server_reads.is_none()));
    row.page_server_reads = Some(Box::new(PageServerReadStatistics {
        availability: PageServerReadAvailability::Available,
        ..Default::default()
    }));
    assert!(
        ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).is_err(),
        "an undeclared capability cannot borrow a native-null verdict"
    );
    capability(&mut source, Some(PageServerReadAvailability::Unavailable));
    row.page_server_reads = Some(Box::new(PageServerReadStatistics {
        availability: PageServerReadAvailability::Unavailable,
        ..Default::default()
    }));
    let unavailable = ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row)).unwrap();
    assert!(
        matches!(&unavailable.record, TelemetryRecord::QueryRuntime(row) if row.page_server_reads.as_ref().unwrap().availability==PageServerReadAvailability::Unavailable)
    );
}
