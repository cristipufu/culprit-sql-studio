use super::*;

async fn fixture(metrics: Option<ReadWorkMetrics>) -> Fixture {
    let (source, records) = test_support::memory_failure_evidence();
    Fixture::with_read_metrics(source, records, test_support::SOURCE_OVERHEAD_QUERY_COUNT, metrics).await
}
fn publication(fixture: &Fixture, rows: Vec<Vec<Value>>) {
    fixture.query.replies.lock().unwrap().push_back(Reply::Rows(rows));
}

#[tokio::test]
async fn metadata_only_read_authenticates_source_totals_and_bounded_metrics_without_native_row_or_payload_reads() {
    for metrics in [
        Some(test_support::source_overhead_read_metrics()),
        None,
        Some(ReadWorkMetrics { active_tasks: 0, untracked_tasks: 0, templates: vec![] }),
    ] {
        let fixture = fixture(metrics.clone()).await;
        publication(&fixture, vec![fixture.publication.clone()]);
        let captured = fixture
            .reader()
            .read_metadata(&fixture.range, EvidenceReadBudget { max_queries: 1, ..Default::default() })
            .await
            .unwrap();
        assert!(captured.is_read_complete());
        assert_eq!(captured.source_queries(), test_support::SOURCE_OVERHEAD_QUERY_COUNT);
        assert_eq!(captured.source_read_metrics(), metrics.as_ref());
        assert_eq!(captured.metrics().queries, 1);
        assert_eq!(captured.metrics().selected_rows, 0);
        assert_eq!(captured.metrics().returned_rows, 0);
        assert_eq!(captured.metrics().pages, 0);
        assert_eq!(captured.metrics().proof_bytes, 0);
        assert_eq!(captured.metrics().recovery_objects, 0);
        assert_eq!(fixture.query.requests.lock().unwrap().len(), 1);
        assert!(
            fixture.query.replies.lock().unwrap().is_empty(),
            "no membership audit or native row request was planned"
        );
        assert!(
            matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall),
            "native payload recovery remains unavailable and unopened"
        );
    }
}

#[tokio::test]
async fn altered_source_query_total_or_read_metrics_cannot_be_substituted_by_the_warehouse() {
    for field in ["SourceQueries", "ReadMetrics"] {
        let mut fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
        let schema = fixture.catalog.publication();
        let column = schema.columns().iter().position(|column| column.name == field).unwrap();
        fixture.publication[column] = if field == "SourceQueries" {
            json!(101)
        } else {
            json!({"active_tasks":0,"untracked_tasks":0,"templates":[]})
        };
        publication(&fixture, vec![fixture.publication.clone()]);
        let error = fixture.reader().read_metadata(&fixture.range, EvidenceReadBudget::default()).await.unwrap_err();
        assert_eq!(
            error.detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{field}: {error:?}"
        );
        assert_eq!(fixture.query.requests.lock().unwrap().len(), 1);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn an_unpublished_completion_is_an_explicit_metadata_gap() {
    let fixture = fixture(Some(test_support::source_overhead_read_metrics())).await;
    publication(&fixture, vec![]);
    let captured = fixture.reader().read_metadata(&fixture.range, EvidenceReadBudget::default()).await.unwrap();
    assert!(!captured.is_read_complete());
    assert_eq!(captured.gaps().len(), 1);
    assert_eq!(captured.gaps()[0].reason, EvidenceGapReason::NotPublished);
    assert_eq!(captured.source_queries(), test_support::SOURCE_OVERHEAD_QUERY_COUNT);
    assert_eq!(captured.metrics().queries, 1);
    assert_eq!(captured.metrics().selected_rows, 0);
}
