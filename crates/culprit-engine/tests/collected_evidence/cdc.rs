use super::*;
use culprit_collector::analytical::test_support::{CdcCase, assert_operational_capture, cdc_evidence};
use culprit_engine::collected_resources;
#[tokio::test]
async fn cdc_originals_keep_all_native_outcomes_and_one_canonical_explanation_without_recovery() {
    for case in CdcCase::ALL {
        let (source, rows) = cdc_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &rows);
        let analysis = collected_resources::cdc(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), rows.len());
        let observations = rows
            .iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let canonical = culprit_checks::cdc::explain(culprit_checks::cdc::CdcEvidence {
            source: &source,
            observations: &observations,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(*analysis.report, canonical);
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(analysis.context.source.target.database().unwrap().incarnation, None);
        if case == CdcCase::Disabled {
            assert_eq!(analysis.report.enabled, Some(false));
            assert!(analysis.report.entries.is_empty());
        }
        if case == CdcCase::UnknownFlag {
            assert_eq!(analysis.report.enabled, None);
        }
        for row in &analysis.context.records {
            if let TelemetryRecord::Operational(row) = &row.row.record {
                if let OperationalFact::MssqlCdc(point) = &row.fact {
                    assert_eq!(point.reset, culprit_core::ResetMarker::Unknown);
                    assert!(point.fits_source(&source));
                }
            }
        }
    }
}
#[tokio::test]
async fn cdc_sdk_refuses_coherent_foreign_vendor_wrong_dataset_and_altered_original_bounds() {
    for change in ["vendor", "dataset", "bounds"] {
        let (mut source, rows) = cdc_evidence(CdcCase::ErrorAndProgress);
        match change {
            "vendor" => {
                let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
                database.engine = culprit_core::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = culprit_core::Engine::Postgres;
            }
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
                header.dataset = SnapshotDataset::Operational;
            }
            _ => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
                header.observed_until = "2020-01-01T00:00:02Z".into();
            }
        }
        assert!(
            rows.iter().any(|row| ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                row.clone()
            )
            .is_err()),
            "{change}"
        );
    }
}
