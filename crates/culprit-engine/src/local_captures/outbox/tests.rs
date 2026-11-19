use super::*;
use culprit_collector::analytical::test_support::{LocalCaptureFixture, page_server_read_evidence_cases};
use culprit_core::{ErrorDetail, limits::OperationGuard, query_store_statement_report::QueryStoreStatementBinding};
mod snapshots;

async fn read(reader: &OutboxCaptureReader, id: &str) -> Result<LocalPageServerReads> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::PageServerReads,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::PageServerReads(result) = result else { panic!("requested page-server analysis") };
    Ok(result)
}

#[tokio::test]
async fn one_canonical_local_capture_reuses_the_archive_view_and_preserves_every_native_case() {
    for case in page_server_read_evidence_cases() {
        let name = case.name;
        let original = case.source.clone();
        let fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete, "{name}");
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = read(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
        let report = result.report();
        assert!(report.read_complete, "{name}");
        assert_eq!(
            report.selected_rows,
            fixture
                .manifest
                .files
                .iter()
                .find(|file| file.stream == TelemetryStream::QueryRuntime)
                .map(|file| file.rows)
        );
        assert_eq!(report.reference_coverage.as_deref(), Some(&fixture.manifest.references));
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.history_from(), original.query_history().unwrap().interval.from);
        assert_eq!(result.history_until(), original.query_history().unwrap().interval.until);
        let runtime = report.runtime_read.as_deref().unwrap();
        assert_eq!(runtime.capture, fixture.manifest.capture);
        if name == "empty" {
            assert!(report.entries.is_empty());
        }
        if name == "native_fragment" {
            assert_eq!(report.entries[0].executions, "9007199254740993");
            assert!(matches!(&report.entries[0].identity,
                culprit_core::page_server_read_report::PageServerReadEntryIdentity::RuntimeFragment { reference, .. }
                if reference.is_informational()));
        }
        if name == "single_fragment_with_definitions" {
            assert_eq!(report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::Captured);
        }
    }
}

#[tokio::test]
async fn only_an_exact_original_definition_binding_may_provide_a_bounded_redacted_preview() {
    let mut case = page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    for record in &mut case.records {
        if let TelemetryRecord::QueryDefinition(definition) = record {
            definition.sql = DefinitionPayload::sql(
                &format!(
                    "SELECT '{}' AS secret, {} FROM dbo.orders WHERE tenant_id=9371",
                    "private-literal-canary",
                    vec!["column_name"; 100].join(",")
                ),
                culprit_core::Engine::Mssql,
            );
        }
    }
    let fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let result = read(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
    let statement = result.report().entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Captured);
    let preview = statement.preview.as_deref().unwrap();
    assert!(preview.chars().count() <= 512);
    assert!(statement.preview_truncated);
    assert!(!preview.contains("private-literal-canary"));
    assert!(!preview.contains("9371"));
}

#[tokio::test]
async fn canonical_verification_and_projection_preserve_the_requested_language_without_changing_native_counts() {
    let case = page_server_read_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let result =
        culprit_core::i18n::scope_async("fr", read(&reader, &fixture.manifest.capture.observation_id)).await.unwrap();
    assert_eq!(
        result.report().title,
        culprit_core::i18n::scope("fr", || culprit_core::t!("view.page_server_reads.title"))
    );
    assert_ne!(
        result.report().title,
        culprit_core::i18n::scope("en", || culprit_core::t!("view.page_server_reads.title"))
    );
    assert_eq!(result.report().entries[0].executions, "9007199254740993");
}

#[tokio::test]
async fn startup_directory_is_lazy_and_never_bypasses_an_existing_owner() {
    let case = page_server_read_evidence_cases().remove(0);
    let mut fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let error = culprit_core::i18n::scope_async("fr", read(&reader, &fixture.manifest.capture.observation_id))
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::Locked)));
    assert_eq!(error.message, culprit_core::i18n::scope("fr", || culprit_core::t!("outbox.locked")));
    drop(fixture.outbox.take());
    let result = read(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
    assert_eq!(result.report().observed_rows, 1);
    let missing = fixture.directory.join("not-created-by-construction");
    let _reader = OutboxCaptureReader::directory(missing.clone(), CompletedCaptureBudget::default()).unwrap();
    assert!(!missing.exists());
}

#[tokio::test]
async fn observed_window_and_unknown_population_metadata_never_become_a_complete_catalog() {
    for observed in [true, false] {
        let mut case = page_server_read_evidence_cases().remove(0);
        let SourceObservation::QueryHistory(history) = &mut case.source.observation else { panic!() };
        let expected = if observed {
            let membership = culprit_core::RuntimePopulationMembership::ObservedWindow {
                from: history.interval.from.clone(),
                until: history.interval.until.clone(),
            };
            history.population_catalog.as_mut().unwrap().membership = membership.clone();
            Some(membership)
        } else {
            history.population_catalog = None;
            history.population_binding = QueryHistoryPopulationBinding::Unknown;
            None
        };
        let fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete);
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = read(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
        assert_eq!(result.population_membership(), expected.as_ref());
        assert_eq!(result.report().entries[0].executions, "9007199254740993");
    }
}

#[tokio::test]
async fn journal_scope_and_queued_cancellation_cannot_return_another_connections_originals() {
    let case = page_server_read_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::query_history(case.source, case.records).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let error = reader
        .analyze(LocalCaptureRequest {
            connection: "different-connection",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::PageServerReads,
            operation: guard.handle(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)));
    let permit = reader.readers.acquire().await.unwrap();
    let operation = guard.handle();
    drop(guard);
    let error = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::PageServerReads,
            operation,
        })
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    drop(permit);
    assert!(read(&reader, &fixture.manifest.capture.observation_id).await.is_ok());
}

mod hyperscale_log_components;
mod memory_clerks;

mod blocking;
mod settlement;

mod session_census;

mod instance_resources;
