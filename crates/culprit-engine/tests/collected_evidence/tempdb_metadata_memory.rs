use super::fixture::Fixture;
use culprit_checks::tempdb_metadata_memory::{TempdbMetadataMemoryEvidence, explain};
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{TempdbMetadataMemoryCase, assert_operational_capture, tempdb_metadata_memory_evidence},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn authenticated_tempdb_metadata_points_preserve_native_scope_nullable_feature_and_exact_bytes_without_recovery()
{
    for case in TempdbMetadataMemoryCase::ALL {
        let (source, records) = tempdb_metadata_memory_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let reads = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &records);
        let analysis = collected_resources::tempdb_metadata_memory(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len(), "independent originals remain retained once");
        let rows = records
            .iter()
            .filter_map(|row| match row {
                TelemetryRecord::Operational(row) if row.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY => {
                    Some(row)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected = explain(TempdbMetadataMemoryEvidence {
            context: &NativeReadContext::from_snapshot(&source).unwrap(),
            coverage: &source.observation.snapshot().unwrap().sources[0],
            observations: &rows,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(analysis.report.as_ref(), &expected);
        if matches!(case, TempdbMetadataMemoryCase::Reserved | TempdbMetadataMemoryCase::NativePartial) {
            assert_eq!(analysis.report.metadata_enabled, Some(true));
            assert_eq!(analysis.report.entries.len(), 1);
            assert_eq!(analysis.report.entries[0].allocated_bytes.as_deref(), Some("9007199254740993"));
            assert_eq!(analysis.report.entries[0].used_bytes.as_deref(), Some("9007199254740992"));
            assert_eq!(
                analysis.report.entries[0].explanation,
                culprit_core::t!("view.tempdb_metadata_memory.usage.reservation")
            );
        } else {
            assert!(analysis.report.entries.is_empty());
            assert_eq!(
                analysis.report.metadata_enabled,
                if case == TempdbMetadataMemoryCase::Disabled { Some(false) } else { None }
            );
        }
        if matches!(case, TempdbMetadataMemoryCase::Unknown | TempdbMetadataMemoryCase::NativePartial) {
            assert_eq!(analysis.report.source_state, SourceState::Partial);
            assert!(
                analysis.report.limitations.contains(&culprit_core::t!("view.tempdb_metadata_memory.limit.partial"))
            );
        }
        for row in &records {
            let original = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(analysis.context.records.iter().any(|record| record.row == original));
        }
        assert_eq!(fixture.recovery_object_reads(), reads);
    }
}
