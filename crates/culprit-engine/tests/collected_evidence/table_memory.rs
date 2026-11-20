use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{TableMemoryCase, assert_operational_capture, table_memory_evidence},
};
use culprit_core::telemetry::*;
#[tokio::test]
async fn selected_table_memory_replays_exact_native_gauges_and_one_canonical_interpretation() {
    for case in TableMemoryCase::ALL {
        let (source, records) = table_memory_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::table_memory(&capture).unwrap();
        let rows = records
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::table_memory::explain(culprit_checks::table_memory::TableMemoryEvidence {
            source: &source,
            observations: &rows,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(analysis.report.observed_rows, records.len() as u32);
    }
}
#[tokio::test]
async fn original_receipts_refuse_saved_object_reuse_foreign_clock_scope_or_edited_native_gauges() {
    let (source, records) = table_memory_evidence(TableMemoryCase::Exact);
    for change in ["object", "creation", "clock", "scope"] {
        let mut altered = source.clone();
        if change == "scope" {
            altered.partition = SourcePartition::Target;
        } else {
            let SourceObservation::TableMemory { target, snapshot, .. } = &mut altered.observation else { panic!() };
            match change {
                "object" => target.object_id += 1,
                "creation" => target.created_without_offset = "2020-01-01T00:00:00.0000000".into(),
                _ => snapshot.observed_until = snapshot.observed_from.clone(),
            }
        }
        let capture = CaptureIdentity { observation_id: "original".into(), revision: None };
        assert!(records.iter().all(|row| ExportedRecord::new(&altered, &capture, row.clone()).is_err()), "{change}");
    }
}
