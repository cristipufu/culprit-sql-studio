use super::*;
#[tokio::test]
async fn selected_columnstore_rowgroups_replay_original_scopes_limits_nulls_and_exact_native_bytes() {
    for case in test_support::ColumnstoreRowgroupCase::ALL {
        let (source, records) = test_support::columnstore_rowgroup_evidence(case);
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::OperationalSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
