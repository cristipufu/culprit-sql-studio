use super::*;
use crate::analytical::test_support::hyperscale_log_component_evidence_cases;

#[tokio::test]
async fn genuine_ready_component_captures_keep_original_native_coverage_all_bodies_and_positions() {
    for (source, rows) in hyperscale_log_component_evidence_cases() {
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let completed = read(&fixture).await.unwrap();
        let references =
            (0..completed.records().len()).map(|index| completed.reference(index).unwrap()).collect::<Vec<_>>();
        assert_eq!(completed.bytes_read(), fixture.original_bytes());
        let capture = completed.capture().clone();
        let input = completed.into_operational_read().unwrap();
        assert!(input.is_valid());
        assert!(input.read_complete);
        assert_eq!(input.description, source);
        assert_eq!(input.origin.capture, capture);
        assert_eq!(input.captured_references().unwrap(), references);
        assert_eq!(
            serde_json::to_value(
                input.observations.iter().cloned().map(TelemetryRecord::Operational).collect::<Vec<_>>()
            )
            .unwrap(),
            serde_json::to_value(&rows).unwrap()
        );
        let original = source.observation.snapshot().unwrap();
        assert_eq!(input.source_coverage(HYPERSCALE_LOG_COMPONENTS_SOURCE), original.sources.first());
        assert_eq!(input.context().unwrap(), NativeReadContext::from_snapshot(&source).unwrap());
    }
}
#[tokio::test]
async fn component_conversion_refuses_rebound_body_namespace_and_ordinal_without_reissuing_positions() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    for change in ["body", "ordinal", "namespace", "source"] {
        let mut completed = read(&fixture).await.unwrap();
        match change {
            "body" => {
                let TelemetryRecord::Operational(row) = &mut completed.records[0].record else { panic!() };
                let OperationalFact::HyperscaleLogComponent(point) = &mut row.fact else { panic!() };
                point.catchup_distance_bytes = "1".into();
            }
            "ordinal" => completed.row_ordinals[0] = 2,
            "namespace" => {
                let CollectionTarget::Database(database) = &mut completed.source.target else { panic!() };
                database.incarnation = Some("different-native-database".into());
            }
            "source" => completed.source.source = "different-reader".into(),
            _ => unreachable!(),
        }
        assert!(completed.into_operational_read().is_err(), "{change}");
    }
}
#[tokio::test]
async fn an_unconfirmed_component_source_never_reaches_the_canonical_completed_converter() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::with_shutdown(source, rows, SourceShutdown::Unconfirmed).await;
    assert_ne!(fixture.manifest.status, CollectionStatus::Complete);
    assert!(read(&fixture).await.is_err());
}
