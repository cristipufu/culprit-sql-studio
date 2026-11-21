use super::*;
use culprit_collector::analytical::test_support::{ChangeFeedCase, assert_operational_capture, change_feed_evidence};
#[tokio::test]
async fn original_change_feed_progress_errors_gaps_and_native_unknowns_replay_without_recovery_or_source_calls() {
    for case in ChangeFeedCase::ALL {
        let (source, rows) = change_feed_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_operational_capture(&capture, &source, &rows);
        let a = culprit_engine::collected_resources::change_feed(&capture).unwrap();
        assert_eq!(a.context.source, source);
        assert_eq!(a.report.context.as_ref().unwrap().target, source.target);
        assert_eq!(a.report.context.as_ref().unwrap().partition, source.partition);
        assert_eq!(a.context.records.len(), rows.len());
        assert_eq!(a.report.sources, source.observation.snapshot().unwrap().sources);
        if case == ChangeFeedCase::Empty {
            assert!(a.report.entries.is_empty());
            assert!(a.report.limitations.contains(&culprit_core::t!("view.change_feed.limit.delivery")));
        }
        if case == ChangeFeedCase::UnknownTask {
            assert!(
                a.report
                    .entries
                    .iter()
                    .filter(|e| e.kind == culprit_core::ChangeCaptureEntryKind::Error)
                    .all(|e| e.fields.iter().any(|f| f.key == "source_task" && f.value.as_deref() == Some("255")))
            );
        }
        assert!(a.context.source.target.database().unwrap().incarnation.is_none());
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}
#[tokio::test]
async fn missing_change_feed_body_retains_original_native_population_and_withdraws_complete_read_claims() {
    let (source, rows) = change_feed_evidence(ChangeFeedCase::ProgressAndError);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 4);
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let a = culprit_engine::collected_resources::change_feed(&capture).unwrap();
    assert_eq!(a.report.source_state, culprit_core::SourceState::Partial);
    assert_eq!(a.report.sources[2].records, 2);
    assert!(!a.report.read_complete);
}
#[tokio::test]
async fn change_feed_cannot_borrow_cdc_dataset_coherent_foreign_engine_or_changed_source_bounds() {
    for change in ["family", "vendor", "bounds"] {
        let (mut source, rows) = change_feed_evidence(ChangeFeedCase::ProgressAndError);
        match change {
            "family" => {
                let SourceObservation::Snapshot(h) = &mut source.observation else { unreachable!() };
                h.dataset = SnapshotDataset::Cdc;
            }
            "vendor" => {
                let CollectionTarget::Database(db) = &mut source.target else { unreachable!() };
                db.engine = culprit_core::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = culprit_core::Engine::Postgres;
            }
            _ => {
                let SourceObservation::Snapshot(h) = &mut source.observation else { unreachable!() };
                h.observed_until = "2020-01-01T00:00:02Z".into();
            }
        }
        assert!(
            rows.into_iter().any(|row| ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                row
            )
            .is_err()),
            "{change}"
        );
    }
}
