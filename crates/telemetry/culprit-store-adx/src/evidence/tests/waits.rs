use super::*;

const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::QueryWaits];

#[tokio::test]
async fn authenticated_wait_metadata_preserves_empty_unknown_ignored_and_unavailable_native_evidence() {
    for (source, records) in test_support::query_wait_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(FAMILIES, 250);
        let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
        assert!(result.is_read_complete(), "{:?}", result.gaps());
        assert_eq!(result.source(), &source);
        let expected =
            records.into_iter().filter(|record| record.stream() == TelemetryStream::QueryWaits).collect::<Vec<_>>();
        assert_eq!(result.records().len(), expected.len());
        for native in expected {
            let exported = ExportedRecord::new(&source, result.capture(), native).unwrap();
            assert!(result.records().iter().any(|actual| actual.row() == &exported));
        }
        if result.records().is_empty() {
            assert_eq!(
                result.metrics().queries,
                2,
                "a published empty batch needs publication and membership proof, no data query"
            );
            assert_eq!(source.query_history().unwrap().waits.as_ref().unwrap().capture, WaitCaptureState::Enabled);
        }
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn a_missing_wait_row_is_a_read_gap_even_with_an_authenticated_enabled_dictionary() {
    let (mut source, records) = test_support::query_evidence();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.waits = Some(test_support::wait_semantics(WaitCaptureState::Enabled));
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.replies(vec![vec![]]);
    let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert_eq!(result.source(), &source);
    assert!(result.records().is_empty());
    assert_eq!(result.gaps().len(), 1);
    assert_eq!(result.gaps()[0].stream, Some(TelemetryStream::QueryWaits));
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::MissingRows);
}

#[tokio::test]
async fn a_changed_published_dictionary_cannot_supply_another_meaning_for_the_committed_capture() {
    let (mut source, records) = test_support::query_evidence();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.waits = Some(test_support::wait_semantics(WaitCaptureState::Enabled));
    history.waits.as_mut().unwrap().coverage = WaitCaptureCoverage::Unverified;
    for changes_coverage in [false, true] {
        let mut fixture = Fixture::new(source.clone(), records.clone()).await;
        let column = fixture
            .catalog
            .publication()
            .columns()
            .iter()
            .position(|column| column.name == "SourceDescription")
            .unwrap();
        let mut description: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        if changes_coverage {
            *description.pointer_mut("/observation/data/waits/coverage").unwrap() = json!("qualified");
        } else {
            *description.pointer_mut("/observation/data/waits/categories/native-page-io/disposition").unwrap() =
                json!({"kind":"analyze","class":"lock"});
        }
        fixture.publication[column] = json!(serde_json::to_string(&description).unwrap());
        fixture.reply_families(FAMILIES, 250);
        let error = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}
