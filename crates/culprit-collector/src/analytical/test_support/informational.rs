//! Native counts remain original facts when their query/plan definitions could not be resolved.
use super::*;

pub fn informational_query_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (source, mut records) = query_evidence();
    let mut runtime = 0;
    for record in &mut records {
        match record {
            TelemetryRecord::QueryRuntime(row) => {
                runtime += 1;
                if runtime == 1 {
                    row.key.references = StatementReferences::default();
                } else if runtime == 2 {
                    row.key.references = StatementReferences {
                        query: Some(NativeDefinitionReference {
                            native: NativeIdentity::new("unresolved-native-query"),
                            definition_id: None,
                        }),
                        plan: Some(NativeDefinitionReference {
                            native: NativeIdentity::new("unresolved-native-plan"),
                            definition_id: None,
                        }),
                    };
                }
            }
            TelemetryRecord::QueryWaits(row) => row.key.references = StatementReferences::default(),
            _ => (),
        }
    }
    assert!(records.iter().all(TelemetryRecord::is_valid));
    (source, records)
}

pub fn assert_informational_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    assert!(capture.reference_coverage().integrity_complete);
    assert!(!capture.reference_coverage().complete);
    assert_eq!(capture.reference_coverage().queries.unresolved, 3);
    assert_eq!(capture.reference_coverage().plans.unresolved, 1);
    assert_eq!(capture.captured_rows(TelemetryStream::QueryRuntime), Some(3));
    assert_eq!(capture.captured_rows(TelemetryStream::QueryWaits), Some(1));
    assert_eq!(capture.metrics().selected_rows, 4);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(capture.records().len(), 4);
    assert_eq!(capture.records().iter().filter(|row| row.row().record_id.is_none()).count(), 3);
    for actual in capture.records() {
        let record = records
            .iter()
            .filter(|row| row.stream() == actual.row().record.stream())
            .nth(actual.row_ordinal() as usize - 1)
            .unwrap();
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        assert_eq!(actual.row(), &expected);
        assert_eq!(actual.native_payload_sha256(), expected.payload_sha256().unwrap());
        if expected.record_id.is_none() {
            assert!(
                DiagnosticEvidenceRef::from_export(
                    source,
                    capture.capture_manifest_sha256(),
                    &expected,
                    actual.row_ordinal() - 1
                )
                .is_err()
            );
        }
        match &actual.row().record {
            TelemetryRecord::QueryRuntime(runtime) => assert_eq!(runtime.executions, "9007199254740993"),
            TelemetryRecord::QueryWaits(wait) => {
                assert_eq!(wait.total, MeasurementValue::Integer("9007199254740993".into()))
            }
            _ => panic!("only selected native measurements are reconstructed"),
        }
    }
}
