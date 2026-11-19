use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{assert_query_cohort_capture, boxed_query_history_evidence},
};
use culprit_core::telemetry::*;
use culprit_engine::collected_resources;
#[tokio::test]
async fn boxed_history_sdk_keeps_query_global_data_local_acquisition_and_original_definition_binding() {
    let (source, records) = boxed_query_history_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let runtime = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let definitions = fixture.read_families(&[EvidenceFamily::QueryDefinitions]).await;
    assert_query_cohort_capture(&runtime, &source, &records);
    assert_query_cohort_capture(&definitions, &source, &records);
    let native_definition = definitions
        .records()
        .iter()
        .find_map(|record| match &record.row().record {
            TelemetryRecord::QueryDefinition(row) => Some(row),
            _ => None,
        })
        .unwrap();
    let native_runtime = runtime
        .records()
        .iter()
        .find_map(|record| match &record.row().record {
            TelemetryRecord::QueryRuntime(row) => Some(row),
            _ => None,
        })
        .unwrap();
    assert!(query_store_query_identity_is_complete(&native_definition.identity));
    assert_eq!(native_definition.id, native_definition.identity.definition_id(&source).unwrap());
    let query = native_runtime.key.references.query.as_ref().unwrap();
    assert_eq!(query.native, native_definition.identity.native());
    assert_eq!(query.definition_id.as_deref(), Some(native_definition.id.as_str()));
    assert!(native_runtime.key.references.is_resolved());
    assert!(
        matches!(&native_definition.sql, DefinitionPayload::Available { text, .. } if !text.is_empty()),
        "the original authenticated definition payload remains available independently of namespace qualification"
    );
    let analysis = collected_resources::page_server_reads(&runtime, Some(&definitions)).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.source.partition, SourcePartition::Target);
    assert!(analysis.context.source.target.database().unwrap().incarnation.is_none());
    let owner = analysis.context.source.query_history().unwrap().acquisition_owner.as_ref().unwrap();
    assert!(owner.has_qualified_local_owner());
    assert_eq!(owner.database_writable, Some(false));
    assert!(NativeSourceReadScope::from_context(owner).is_err());
    let entry = &analysis.report.entries[0];
    assert_eq!(entry.executions, "9007199254740993");
    let statement = entry.statement.as_ref().unwrap();
    assert_eq!(statement.binding, culprit_core::query_store_statement_report::QueryStoreStatementBinding::Captured);
    assert!(
        statement.definition.is_some() && statement.preview.as_deref().is_some_and(|sql| !sql.is_empty()),
        "one original capture may bind its own SQL without qualifying a global DATA namespace"
    );
    let original_definition = &definitions.records()[0];
    let reference = DiagnosticEvidenceRef::from_export(
        definitions.source(),
        definitions.capture_manifest_sha256(),
        original_definition.row(),
        original_definition.row_ordinal() - 1,
    )
    .unwrap();
    assert_eq!(statement.definition.as_ref(), Some(&reference));
    let definition_read = analysis.report.definition_read.as_ref().unwrap();
    assert_eq!(
        definition_read.namespace,
        culprit_core::query_store_statement_report::QueryStoreDefinitionNamespace::Unverified
    );
    assert_eq!(definition_read.selected_rows, 1);
    assert_eq!(definition_read.observed_rows, 1);
    assert!(definition_read.read_complete);
    assert!(query_store_database_namespace(&source.target, &source.partition).is_none());
    assert_eq!(analysis.definition_context.as_ref().unwrap().source, source);
    assert_eq!(fixture.recovery_object_reads(), opens);
    assert!(analysis.report.read_complete);
}
#[tokio::test]
async fn an_archived_missing_boxed_runtime_body_keeps_the_original_owner_and_explicit_gap() {
    let (source, records) = boxed_query_history_evidence();
    let fixture = Fixture::new(source.clone(), records).await;
    let opens = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::QueryRuntime);
    let capture = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    assert!(!capture.is_read_complete());
    assert_eq!(capture.metrics().selected_rows, 1);
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    let report = collected_resources::page_server_reads(&capture, None).unwrap();
    assert!(!report.report.read_complete);
    assert_eq!(report.context.source, source);
    assert_eq!(fixture.recovery_object_reads(), opens);
}

#[tokio::test]
async fn an_independently_captured_boxed_companion_cannot_borrow_sql_from_a_qualified_local_owner() {
    let (source, records) = boxed_query_history_evidence();
    let runtime_fixture = Fixture::with_sequence(source.clone(), records.clone(), 956_000).await;
    let definition_fixture = Fixture::with_sequence(source.clone(), records, 956_001).await;
    let runtime_opens = runtime_fixture.recovery_object_reads();
    let definition_opens = definition_fixture.recovery_object_reads();
    let runtime = runtime_fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    let definitions = definition_fixture.read_families(&[EvidenceFamily::QueryDefinitions]).await;
    assert_eq!(runtime.source(), definitions.source());
    assert_ne!(runtime.capture(), definitions.capture());
    let analysis = collected_resources::page_server_reads(&runtime, Some(&definitions)).unwrap();
    let statement = analysis.report.entries[0].statement.as_ref().unwrap();
    assert_eq!(
        statement.binding,
        culprit_core::query_store_statement_report::QueryStoreStatementBinding::SourceUnverified
    );
    assert!(statement.preview.is_none() && statement.definition.is_none());
    assert_eq!(
        analysis.report.definition_read.as_ref().unwrap().namespace,
        culprit_core::query_store_statement_report::QueryStoreDefinitionNamespace::Unverified
    );
    assert_eq!(analysis.context.source, source);
    assert!(
        analysis
            .context
            .source
            .query_history()
            .unwrap()
            .acquisition_owner
            .as_ref()
            .unwrap()
            .has_qualified_local_owner()
    );
    assert!(query_store_database_namespace(&source.target, &source.partition).is_none());
    assert_eq!(runtime_fixture.recovery_object_reads(), runtime_opens);
    assert_eq!(definition_fixture.recovery_object_reads(), definition_opens);
}
