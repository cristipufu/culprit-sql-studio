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
        Some(test_support::native_source_overhead_read_metrics()),
        Some(test_support::connection_source_overhead_read_metrics()),
        None,
        Some(ReadWorkMetrics {
            connection_work: None,
            native_session_work: None,
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![],
        }),
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
        if let Some(metrics) = metrics.as_ref().filter(|metrics| metrics.native_session_work.is_some()) {
            let schema = fixture.catalog.publication();
            let index = schema.columns().iter().position(|column| column.name == "ReadMetrics").unwrap();
            let public = &fixture.publication[index];
            let public = public
                .as_str()
                .map(|text| serde_json::from_str::<Value>(text).unwrap())
                .unwrap_or_else(|| public.clone());
            test_support::assert_native_source_overhead_summary(&public, metrics);
        }
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
            json!({"connection_work":null,"active_tasks":0,"untracked_tasks":0,"templates":[],"native_session_work":null})
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

#[tokio::test]
async fn numeric_native_summary_or_opaque_proof_tampering_is_not_authenticated_as_original_source_work() {
    for field in ["reference", "cpu_ms", "state"] {
        let mut fixture = fixture(Some(test_support::native_source_overhead_read_metrics())).await;
        let column =
            fixture.catalog.publication().columns().iter().position(|column| column.name == "ReadMetrics").unwrap();
        let original = &fixture.publication[column];
        let mut altered = original
            .as_str()
            .map(|text| serde_json::from_str::<Value>(text).unwrap())
            .unwrap_or_else(|| original.clone());
        match field {
            "reference" => altered["native_session_work"]["reference"] = json!(digest(b"replacement proof")),
            "cpu_ms" => altered["native_session_work"]["interval"]["cpu_ms"] = json!("8"),
            "state" => altered["native_session_work"]["state"] = json!("observing"),
            _ => unreachable!(),
        }
        fixture.publication[column] =
            if original.is_string() { json!(serde_json::to_string(&altered).unwrap()) } else { altered };
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
async fn original_acquisition_codes_cannot_be_changed_into_a_successful_login_by_publication() {
    let original = test_support::connection_source_overhead_read_metrics();
    let mut fixture = fixture(Some(original)).await;
    let schema = fixture.catalog.publication();
    let column = schema.columns().iter().position(|column| column.name == "ReadMetrics").unwrap();
    let public = &fixture.publication[column];
    let mut public =
        public.as_str().map(|text| serde_json::from_str::<Value>(text).unwrap()).unwrap_or_else(|| public.clone());
    assert_eq!(public["connection_work"]["attempts"][0]["phases"][1]["outcome"]["native_code"], 18456);
    public["connection_work"]["attempts"][0]["phases"][1]["outcome"] = json!({"kind":"completed"});
    fixture.publication[column] =
        if fixture.publication[column].is_string() { json!(serde_json::to_string(&public).unwrap()) } else { public };
    publication(&fixture, vec![fixture.publication.clone()]);
    let error = fixture
        .reader()
        .read_metadata(&fixture.range, EvidenceReadBudget { max_queries: 1, ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.query.requests.lock().unwrap().len(), 1);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}
