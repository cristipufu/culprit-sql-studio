use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{FullTextPopulationCase, assert_operational_capture, full_text_population_evidence},
};
use culprit_core::telemetry::*;

#[tokio::test]
async fn selected_runtime_populations_replay_the_same_original_rows_and_gaps_without_recovery_or_current_metadata() {
    for case in FullTextPopulationCase::ALL {
        let (source, records) = full_text_population_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::full_text_populations(&capture).unwrap();
        let observations = records
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::full_text_populations::explain(
            culprit_checks::full_text_populations::FullTextPopulationEvidence {
                source: &source,
                observations: &observations,
                read_complete: true,
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.observed_rows, records.len() as u32);
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[test]
fn target_generation_role_scope_or_original_point_changes_cannot_borrow_an_authenticated_row() {
    let (source, records) = full_text_population_evidence(FullTextPopulationCase::Processing);
    for changed in ["catalog", "table", "created", "clock", "role", "scope"] {
        let mut altered = source.clone();
        if changed == "scope" {
            altered.partition = SourcePartition::Target;
        } else {
            let SourceObservation::FullTextPopulations { target, snapshot, database_writable, .. } =
                &mut altered.observation
            else {
                panic!()
            };
            match changed {
                "catalog" => target.catalog_id += 1,
                "table" => target.table.object_id += 1,
                "created" => target.table.created_without_offset = "2019-01-02T00:00:00.000".into(),
                "clock" => snapshot.observed_until = snapshot.observed_from.clone(),
                "role" => *database_writable = false,
                _ => unreachable!(),
            }
        }
        assert!(
            records.iter().all(|row| ExportedRecord::new(
                &altered,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                row.clone()
            )
            .is_err()),
            "{changed}"
        );
    }
}
