use super::*;

#[tokio::test]
async fn actual_replica_members_epochs_roles_and_unknown_partial_points_round_trip_without_changing_the_database() {
    let cases = test_support::replica_activity_evidence_cases();
    let (first, _) = &cases[0];
    for (index, (source, records)) in cases.iter().enumerate() {
        assert_eq!(source.target.telemetry_id().unwrap(), first.target.telemetry_id().unwrap());
        if index == 1 {
            assert_ne!(source.logical_source_id().unwrap(), first.logical_source_id().unwrap());
        } else if index == 2 {
            assert_eq!(source.logical_source_id().unwrap(), first.logical_source_id().unwrap());
            assert_ne!(source.captured_range().unwrap().scope_id, first.captured_range().unwrap().scope_id);
        } else if index == 3 {
            assert_eq!(source.captured_range().unwrap().scope_id, first.captured_range().unwrap().scope_id);
        }
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ActivityObservations).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::ActivityObservations],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_replica_activity_capture(&capture, source, records);
        let schema = fixture.catalog.get(TelemetryStream::ActivityObservations).unwrap();
        let logical = schema.columns().iter().position(|column| column.name == "LogicalSourceId").unwrap();
        assert!(fixture.rows.iter().all(|row| row[logical] == json!(source.logical_source_id().unwrap())));
        let publication = serde_json::to_string(&fixture.publication).unwrap();
        assert!(!publication.contains("private-compute-"));
    }
}

#[tokio::test]
async fn replicated_history_uses_the_same_native_rows_across_read_members_and_local_scope_tampering_fails_proof() {
    let mut expected = std::collections::BTreeMap::new();
    for (source, records) in test_support::replicated_history_evidence_cases() {
        for family in [
            EvidenceFamily::QueryDefinitions,
            EvidenceFamily::PlanDefinitions,
            EvidenceFamily::QueryRuntime,
            EvidenceFamily::QueryWaits,
        ] {
            let fixture = Fixture::new_for_stream(source.clone(), records.clone(), family.stream()).await;
            fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
            let capture = fixture
                .reader
                .read_capture(
                    &fixture.range,
                    &[family],
                    EvidenceReadBudget { page_rows: 1, ..Default::default() },
                    None,
                )
                .await
                .unwrap();
            assert!(capture.is_read_complete(), "{:?}", capture.gaps());
            let ids = capture.records().iter().map(|record| record.row().record_id.clone()).collect::<Vec<_>>();
            let stream = family.stream().file_name();
            if let Some((logical, scope, previous)) = expected.get(stream) {
                assert_eq!(&source.logical_source_id().unwrap(), logical);
                assert_eq!(&source.captured_range().unwrap().scope_id, scope);
                assert_eq!(&ids, previous);
            }
            expected
                .insert(stream, (source.logical_source_id().unwrap(), source.captured_range().unwrap().scope_id, ids));
        }
    }
    let cases = test_support::replica_activity_evidence_cases();
    let (source, records) = &cases[0];
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::ActivityObservations).await;
    let schema = fixture.catalog.get(TelemetryStream::ActivityObservations).unwrap();
    for (field, replacement) in [
        ("SourceId", cases[1].0.captured_range().unwrap().scope_id),
        ("LogicalSourceId", cases[1].0.logical_source_id().unwrap()),
    ] {
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        let mut changed = fixture.rows[0].clone();
        changed[column] = json!(replacement);
        fixture.replies(vec![vec![changed]]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::ActivityObservations], EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
    }
}
