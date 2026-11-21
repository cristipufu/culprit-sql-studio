use super::*;
#[tokio::test]
async fn mi_interval_projection_preserves_original_scope_wide_values_and_partial_or_unavailable_coverage() {
    for case in test_support::ManagedInstanceResourceCase::ALL {
        let (source, records) = test_support::managed_instance_resource_case(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(std::sync::atomic::Ordering::SeqCst);
        fixture.reply_families(&[EvidenceFamily::ResourceSamples], 1);
        let capture = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::ResourceSamples],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(std::sync::atomic::Ordering::SeqCst), opened);
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.captured_rows(TelemetryStream::ResourceSamples), Some(records.len() as u32));
        if let Some(row) = capture.records().first() {
            let TelemetryRecord::ResourceSample(native) = &row.row().record else { panic!() };
            assert!(native.replica_id.is_none() && native.replica_role.is_none());
            assert_eq!(
                native.mssql_managed_instance.as_ref().unwrap().io_bytes_read.as_deref(),
                Some("9223372036854775807")
            );
        }
    }
}
