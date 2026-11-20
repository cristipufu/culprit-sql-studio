use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{ColumnstoreRowgroupCase, assert_operational_capture, columnstore_rowgroup_evidence},
};
use culprit_core::telemetry::*;
#[tokio::test]
async fn selected_rowgroups_replay_original_owner_exact_quantities_and_one_canonical_explanation() {
    for case in ColumnstoreRowgroupCase::ALL {
        let (source, records) = columnstore_rowgroup_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::columnstore_rowgroups(&capture).unwrap();
        let rows = records
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::columnstore_rowgroups::explain(
            culprit_checks::columnstore_rowgroups::ColumnstoreRowgroupEvidence {
                source: &source,
                observations: &rows,
                read_complete: true,
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[tokio::test]
async fn selected_rowgroup_analysis_refuses_foreign_target_and_source_clock_instead_of_borrowing_rows() {
    let (source, records) = columnstore_rowgroup_evidence(ColumnstoreRowgroupCase::States);
    for change in ["target", "clock", "scope"] {
        let mut altered = source.clone();
        if change == "scope" {
            altered.partition = SourcePartition::Target;
        } else {
            let SourceObservation::ColumnstoreRowgroups { target, snapshot, .. } = &mut altered.observation else {
                panic!()
            };
            if change == "target" {
                target.index_id = 2;
            } else {
                snapshot.observed_until = snapshot.observed_from.clone();
            }
        }
        let capture = CaptureIdentity { observation_id: "original".into(), revision: None };
        assert!(records.iter().all(|row| ExportedRecord::new(&altered, &capture, row.clone()).is_err()), "{change}");
    }
}
