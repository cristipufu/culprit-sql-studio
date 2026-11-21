use super::*;

struct Original {
    source: CollectionDescription,
    parent: CapturedReadRef,
    row: ExportedRecord,
}
impl Original {
    fn runtime(resolved: bool, rows: u32) -> Self {
        let source = CollectionDescription {
            target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "app", "fixture.invalid", 1433, None).into(),
            partition: SourcePartition::Target,
            source: "original-query-history".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::QueryHistory(QueryHistoryDescription {
                acquisition_owner: None,
                interval: TelemetryInterval {
                    native_id: "1".into(),
                    from: "2020-01-01T00:00:00Z".into(),
                    until: "2020-01-01T01:00:00Z".into(),
                },
                population_catalog: None,
                population_binding: QueryHistoryPopulationBinding::Unknown,
                waits: None,
                dimensions: Default::default(),
                native_properties: Default::default(),
            }),
        };
        let capture = CaptureIdentity {
            observation_id: "original-capture".into(),
            revision: Some(CaptureRevision {
                outbox_id: "original-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let query = NativeQueryIdentity { query_id: "42".into(), qualifiers: Default::default() };
        let references = if resolved {
            StatementReferences {
                query: Some(NativeDefinitionReference {
                    native: query.native(),
                    definition_id: Some(query.definition_id(&source).unwrap()),
                }),
                plan: None,
            }
        } else {
            StatementReferences::default()
        };
        let row = ExportedRecord::new(
            &source,
            &capture,
            TelemetryRecord::QueryRuntime(RuntimeMeasurement {
                key: NativeMeasurementKey { native: NativeIdentity::new("17"), references, cohort: None },
                executions: "9007199254740993".into(),
                metrics: vec![],
                page_server_reads: None,
                first_execution_at: Some("2020-01-01T00:01:00.123456789Z".into()),
                last_execution_at: None,
            }),
        )
        .unwrap();
        let parent = CapturedReadRef::new(
            &source,
            capture,
            digest(b"original-manifest"),
            vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows }],
        )
        .unwrap();
        Self { source, parent, row }
    }

    fn reference(&self, ordinal: u32) -> Result<CapturedRecordReference> {
        CapturedRecordReference::from_export(&self.parent, &self.source, &self.row, ordinal)
    }
}

#[test]
fn unresolved_originals_keep_positions_and_exact_counts_without_a_diagnostic_identity() {
    let original = Original::runtime(false, 3);
    assert!(original.row.record_id.is_none());
    assert!(
        DiagnosticEvidenceRef::from_export(&original.source, &original.parent.manifest_sha256, &original.row, 2)
            .is_err()
    );
    let reference = original.reference(2).unwrap();
    assert!(reference.is_informational());
    let CapturedRecordReference::Informational { position } = &reference else { panic!() };
    assert_eq!(position.row_ordinal, 2, "a partial body keeps the original position, not its vector index");
    assert_eq!(position.payload_sha256, original.row.payload_sha256().unwrap());
    reference.validate_canonical_record(&original.parent, &original.source, &original.row.record).unwrap();
    let json = serde_json::to_value(&reference).unwrap();
    assert_eq!(json["kind"], "informational");
    assert!(json.get("record_id").is_none() && json.get("native_key").is_none());
    let TelemetryRecord::QueryRuntime(runtime) = &original.row.record else { panic!() };
    assert_eq!(runtime.executions, "9007199254740993");
    assert!(original.reference(3).is_err());
}

#[test]
fn resolved_originals_require_the_existing_strict_reference_and_never_fall_back() {
    let mut original = Original::runtime(true, 1);
    assert!(matches!(original.reference(0).unwrap(), CapturedRecordReference::Diagnostic { .. }));
    for record_id in [None, Some(digest(b"different-semantic-id"))] {
        original.row.record_id = record_id;
        assert!(original.reference(0).is_err());
    }
    let original = Original::runtime(true, 1);
    let informational = CapturedRecordReference::Informational {
        position: CapturedPosition {
            stream: TelemetryStream::QueryRuntime,
            row_ordinal: 0,
            payload_sha256: original.row.payload_sha256().unwrap(),
        },
    };
    assert!(informational.validate_canonical_record(&original.parent, &original.source, &original.row.record).is_err());
}

#[test]
fn original_bytes_namespace_and_capture_are_not_repaired_or_substituted() {
    let original = Original::runtime(false, 1);
    let reference = original.reference(0).unwrap();
    for change in 0..7 {
        let mut parent = original.parent.clone();
        let mut source = original.source.clone();
        let mut record = original.row.record.clone();
        match change {
            0 => {
                let TelemetryRecord::QueryRuntime(runtime) = &mut record else { panic!() };
                runtime.executions = "0".into();
            }
            1 => {
                let TelemetryRecord::QueryRuntime(runtime) = &mut record else { panic!() };
                runtime.first_execution_at = Some("2020-01-01T00:01:00.123456789+00:00".into());
            }
            2 => source.source = "foreign-source".into(),
            3 => parent.capture.observation_id = "another-capture".into(),
            4 => parent.source_sha256 = digest(b"another-description"),
            5 => {
                let CollectionTarget::Database(database) = &mut source.target else { panic!() };
                database.incarnation = Some("another-incarnation".into());
            }
            _ => parent.streams[0].rows = 0,
        }
        assert!(reference.validate_canonical_record(&parent, &source, &record).is_err(), "change {change}");
    }
    let mut rewritten = original.row.clone();
    let TelemetryRecord::QueryRuntime(runtime) = &mut rewritten.record else { panic!() };
    runtime.first_execution_at = Some("2020-01-01T00:01:00.123456789+00:00".into());
    assert!(CapturedRecordReference::from_export(&original.parent, &original.source, &rewritten, 0).is_err());
}

#[test]
fn original_stream_counts_distinguish_empty_uncaptured_and_missing_bodies() {
    let mut original = Original::runtime(false, 0);
    assert_eq!(original.parent.captured_rows(TelemetryStream::QueryRuntime), Some(0));
    assert_eq!(original.parent.captured_rows(TelemetryStream::QueryWaits), None);
    assert!(original.reference(0).is_err());
    original.parent.streams.push(original.parent.streams[0].clone());
    assert!(original.parent.validate_source(&original.source).is_err());
    let mut original = Original::runtime(false, 1);
    original.parent.streams[0].stream = TelemetryStream::ActivityObservations;
    assert!(original.parent.validate_source(&original.source).is_err());
}

#[test]
fn invalid_resolved_references_and_informational_stream_changes_are_rejected() {
    let original = Original::runtime(true, 1);
    let CapturedRecordReference::Diagnostic { mut reference } = original.reference(0).unwrap() else { panic!() };
    reference.record_id = digest(b"forged-semantic-id");
    let reference = CapturedRecordReference::Diagnostic { reference };
    assert!(reference.validate_canonical_record(&original.parent, &original.source, &original.row.record).is_err());
    let original = Original::runtime(false, 1);
    let CapturedRecordReference::Informational { mut position } = original.reference(0).unwrap() else { panic!() };
    position.stream = TelemetryStream::QueryWaits;
    let reference = CapturedRecordReference::Informational { position };
    assert!(reference.validate_canonical_record(&original.parent, &original.source, &original.row.record).is_err());
}

#[test]
fn original_export_envelopes_are_validated_before_an_informational_position_is_returned() {
    let original = Original::runtime(false, 1);
    for change in 0..5 {
        let mut row = original.row.clone();
        match change {
            0 => row.capture.revision.as_mut().unwrap().owner_epoch = "2".into(),
            1 => row.batch_id = digest(b"another-batch"),
            2 => row.format_version += 1,
            3 => row.representation_version += 1,
            _ => row.record_id = Some(digest(b"invented-semantic-id")),
        }
        assert!(
            CapturedRecordReference::from_export(&original.parent, &original.source, &row, 0).is_err(),
            "change {change}"
        );
    }
}

#[test]
fn unresolved_waits_keep_their_original_unit_and_position_too() {
    let mut original = Original::runtime(false, 1);
    original.parent.streams[0].stream = TelemetryStream::QueryWaits;
    original.row = ExportedRecord::new(
        &original.source,
        &original.parent.capture,
        TelemetryRecord::QueryWaits(WaitMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new("native-wait-fragment"),
                references: StatementReferences::default(),
                cohort: None,
            },
            category: "Lock".into(),
            unit: RuntimeMetricUnit::Milliseconds,
            total: MeasurementValue::Integer("9007199254740993".into()),
            average: None,
            minimum: None,
            maximum: None,
            last: None,
            standard_deviation: None,
            category_description: None,
            outcome_description: None,
        }),
    )
    .unwrap();
    let reference = original.reference(0).unwrap();
    assert!(reference.is_informational());
    reference.validate_canonical_record(&original.parent, &original.source, &original.row.record).unwrap();
}
