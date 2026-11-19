use super::*;
use culprit_core::page_server_read_report::PageServerReadReport;

fn captured_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let CollectionTarget::Database(database) = &mut fixture.runtime_source.target else { panic!() };
    database.incarnation = None;
    fixture.definition_source = fixture.runtime_source.clone();
    fixture.compiler_context.target = fixture.runtime_source.target.clone();
    fixture.seal();
    fixture.runtime.key.references.query.as_mut().unwrap().definition_id = Some(fixture.definitions[0].id.clone());
    fixture
}

fn runtime_read(fixture: &Fixture) -> CapturedReadRef {
    let mut read = fixture.definition_read(1).unwrap();
    read.streams[0].stream = TelemetryStream::QueryRuntime;
    read
}

fn rows<'a>(fixture: &'a Fixture, read: &CapturedReadRef) -> Vec<QueryStoreDefinitionRecord<'a>> {
    fixture
        .definitions
        .iter()
        .enumerate()
        .map(|(ordinal, definition)| {
            let record = TelemetryRecord::QueryDefinition(definition.clone());
            let exported = ExportedRecord::new(&fixture.definition_source, &read.capture, record.clone()).unwrap();
            assert_eq!(exported.record, record, "only original canonical records enter the binding");
            QueryStoreDefinitionRecord {
                reference: DiagnosticEvidenceRef::from_export(
                    &fixture.definition_source,
                    &read.manifest_sha256,
                    &exported,
                    ordinal as u32,
                )
                .unwrap(),
                definition,
            }
        })
        .collect()
}

fn statement(report: &PageServerReadReport) -> &QueryStoreStatementExplanation {
    report.entries[0].statement.as_deref().unwrap()
}

#[test]
fn an_original_capture_supplies_its_exact_statement_without_qualifying_cross_capture_database_identity() {
    let fixture = captured_fixture();
    let rows = fixture.rows();
    let original = fixture.runtime.clone();
    let report = fixture.runtime_report(&rows, 1, true).unwrap();
    let statement = statement(&report);
    assert_eq!(statement.binding, QueryStoreStatementBinding::Captured);
    assert_eq!(statement.definition.as_ref(), Some(&rows[0].reference));
    assert_eq!(statement.origin, StatementOrigin::Application);
    assert!(statement.preview.as_deref().unwrap().contains("dbo.orders"));
    assert!(!statement.preview.as_deref().unwrap().contains("81724"));
    assert_eq!(report.definition_read.as_ref().unwrap().namespace, QueryStoreDefinitionNamespace::Unverified);
    assert!(
        query_store_database_namespace(&fixture.runtime_source.target, &fixture.runtime_source.partition).is_none()
    );
    assert_eq!(fixture.runtime, original);
    assert_eq!(report.entries[0].executions, "9007199254740993");
    assert!(report.read_complete);
    assert!(report.limitations.contains(&culprit_core::t!("view.page_server_reads.statement.limit.captured")));
    assert!(!report.limitations.contains(&culprit_core::t!("view.page_server_reads.statement.unverified")));
    assert!(!statement.explanation.contains("compiler"));
    let compiler = fixture.compiler_report(&rows, 1, true).unwrap();
    assert_eq!(compiler.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::SourceUnverified);
}

#[test]
fn the_same_source_native_identity_and_sql_in_another_capture_stay_unverified() {
    let fixture = captured_fixture();
    let runtime = runtime_read(&fixture);
    let mut definitions = fixture.definition_read(1).unwrap();
    definitions.capture.observation_id = "different-original-capture".into();
    definitions.capture.revision.as_mut().unwrap().sequence = "2".into();
    definitions.manifest_sha256 = digest(b"different-original-manifest");
    let rows = rows(&fixture, &definitions);
    let report = fixture.runtime_report_with_reads(&rows, 1, true, &runtime, &definitions).unwrap();
    assert_eq!(statement(&report).binding, QueryStoreStatementBinding::SourceUnverified);
    assert!(statement(&report).definition.is_none() && statement(&report).preview.is_none());
    assert_eq!(report.entries[0].executions, "9007199254740993");
    assert_eq!(report.definition_read.as_ref().unwrap().namespace, QueryStoreDefinitionNamespace::Unverified);
}

#[test]
fn a_same_capture_claim_requires_both_original_parents_and_their_unchanged_manifest_and_source() {
    for field in ["missing-manifest", "missing-source", "missing-stream", "manifest", "source", "counts"] {
        let fixture = captured_fixture();
        let runtime = runtime_read(&fixture);
        let mut definitions = fixture.definition_read(1).unwrap();
        let original_rows = fixture.rows();
        match field {
            "missing-manifest" => definitions.manifest_sha256.clear(),
            "missing-source" => definitions.source_sha256.clear(),
            "missing-stream" => definitions.streams.clear(),
            "manifest" => definitions.manifest_sha256 = digest(b"foreign-definition-manifest"),
            "source" => definitions.source_sha256 = digest(b"foreign-definition-source"),
            _ => definitions.streams[0].rows = 2,
        }
        assert!(fixture.runtime_report_with_reads(&original_rows, 1, true, &runtime, &definitions).is_err(), "{field}");
        assert!(fixture.runtime_report_with_reads(&[], 1, false, &runtime, &definitions).is_err(), "empty {field}");
    }
}

#[test]
fn an_identical_capture_tuple_cannot_hide_a_different_target_source_interval_or_native_population() {
    for field in ["database", "source", "interval", "native-property", "population"] {
        let mut fixture = captured_fixture();
        let runtime = runtime_read(&fixture);
        match field {
            "database" => {
                let CollectionTarget::Database(database) = &mut fixture.definition_source.target else { panic!() };
                database.database = "different-original-database".into();
            }
            "source" => fixture.definition_source.source = "different-original-source".into(),
            field => {
                let SourceObservation::QueryHistory(history) = &mut fixture.definition_source.observation else {
                    panic!()
                };
                match field {
                    "interval" => history.interval.native_id = "different-original-interval".into(),
                    "native-property" => {
                        history.native_properties.insert("original-layout".into(), "different".into());
                    }
                    _ => {
                        history.dimensions.insert("execution_type".into(), vec!["0".into(), "3".into()]);
                    }
                }
            }
        }
        fixture.seal();
        let mut definitions = fixture.definition_read(1).unwrap();
        definitions.capture = runtime.capture.clone();
        definitions.manifest_sha256 = runtime.manifest_sha256.clone();
        let rows = rows(&fixture, &definitions);
        assert!(fixture.runtime_report_with_reads(&rows, 1, true, &runtime, &definitions).is_err(), "{field}");
    }
}

#[test]
fn a_captured_statement_requires_the_original_resolved_reference_and_every_native_incarnation_field() {
    for field in ["reference", "query", "context_settings_id", "object_id", "query_hash", "initial_compile_at"] {
        let mut fixture = captured_fixture();
        if field == "query" {
            fixture.runtime.key.references.query = None;
        } else {
            let query = fixture.runtime.key.references.query.as_mut().unwrap();
            query.definition_id = None;
            if field != "reference" {
                query.native.qualifiers.remove(field);
            }
        }
        let report = fixture.runtime_report(&fixture.rows(), 1, true).unwrap();
        assert_eq!(statement(&report).binding, QueryStoreStatementBinding::IdentityUnavailable, "{field}");
        assert!(statement(&report).preview.is_none() && statement(&report).definition.is_none());
        assert_eq!(report.entries[0].executions, "9007199254740993");
    }
    let mut fixture = captured_fixture();
    fixture.runtime.key.references.query.as_mut().unwrap().definition_id = Some(digest(b"stale-original-definition"));
    assert!(fixture.runtime_report(&fixture.rows(), 1, true).is_err());
}

#[test]
fn changed_native_incarnations_and_receipts_never_repair_the_original_reference() {
    for field in ["context_settings_id", "object_id", "query_hash", "initial_compile_at", "native-handle"] {
        let mut fixture = captured_fixture();
        let definition = &mut fixture.definitions[0];
        let value = match field {
            "context_settings_id" => "6",
            "object_id" => "1",
            "query_hash" => "1123456789ABCDEF",
            "initial_compile_at" => "2020-01-01T05:00:01Z",
            _ => "another-native-handle",
        };
        definition.identity.qualifiers.insert(field.into(), value.into());
        fixture.seal();
        let report = fixture.runtime_report(&fixture.rows(), 1, true).unwrap();
        assert_eq!(statement(&report).binding, QueryStoreStatementBinding::NotObserved, "{field}");
        assert!(statement(&report).preview.is_none());
    }
    let fixture = captured_fixture();
    let mut rows = fixture.rows();
    rows[0].reference.payload_sha256 = digest(b"foreign-canonical-body");
    assert!(fixture.runtime_report(&rows, 1, true).is_err());
}

#[test]
fn missing_bodies_and_unavailable_payloads_keep_binding_completeness_and_native_counts_separate() {
    let fixture = captured_fixture();
    let partial = fixture.runtime_report(&[], 1, false).unwrap();
    assert_eq!(statement(&partial).binding, QueryStoreStatementBinding::NotObserved);
    assert!(statement(&partial).preview.is_none());
    assert!(partial.read_complete);
    let definitions = partial.definition_read.as_ref().unwrap();
    assert_eq!((definitions.selected_rows, definitions.observed_rows, definitions.read_complete), (1, 0, false));
    assert!(partial.limitations.contains(&culprit_core::t!("view.page_server_reads.statement.limit.captured_partial")));
    for reason in [PayloadUnavailable::Missing, PayloadUnavailable::Restricted, PayloadUnavailable::RedactionFailed] {
        let mut fixture = captured_fixture();
        fixture.definitions[0].sql = DefinitionPayload::Unavailable { reason };
        let rows = fixture.rows();
        let report = fixture.runtime_report(&rows, 1, true).unwrap();
        assert_eq!(statement(&report).binding, QueryStoreStatementBinding::Captured);
        assert_eq!(statement(&report).definition.as_ref(), Some(&rows[0].reference));
        assert_eq!(statement(&report).unavailable, Some(reason));
        assert!(statement(&report).preview.is_none());
        assert!(report.definition_read.as_ref().unwrap().read_complete);
        assert_eq!(report.entries[0].executions, "9007199254740993");
    }
}

#[test]
fn captured_origin_and_unicode_preview_use_the_existing_payload_and_character_limit() {
    for origin in [StatementOrigin::Application, StatementOrigin::CulpritMonitoring, StatementOrigin::EngineInternal] {
        let mut fixture = captured_fixture();
        let definition = &mut fixture.definitions[0];
        definition.origin = origin;
        definition.monitoring_template_id =
            (origin == StatementOrigin::CulpritMonitoring).then(|| digest(b"original-capture-monitoring-template"));
        definition.sql = DefinitionPayload::sql(
            &format!(
                "SELECT 'private-captured-canary' AS secret, [{}] FROM dbo.orders OPTION(RECOMPILE)",
                "界".repeat(600)
            ),
            Engine::Mssql,
        );
        let expected_template = definition.monitoring_template_id.clone();
        let rows = fixture.rows();
        let report = fixture.runtime_report(&rows, 1, true).unwrap();
        let statement = statement(&report);
        assert_eq!(statement.binding, QueryStoreStatementBinding::Captured);
        assert_eq!(statement.origin, origin);
        assert_eq!(statement.monitoring_template_id, expected_template);
        assert!(statement.preview_truncated);
        let preview = statement.preview.as_deref().unwrap();
        assert_eq!(preview.chars().count(), STATEMENT_PREVIEW_CHARACTERS);
        assert!(preview.len() > STATEMENT_PREVIEW_CHARACTERS);
        assert!(!preview.contains("private-captured-canary"));
    }
}

#[test]
fn duplicated_physical_semantic_and_native_incarnations_are_refused_in_one_capture() {
    let fixture = captured_fixture();
    let original = fixture.rows();
    for ordinal in [0, 1] {
        let mut second = original[0].reference.clone();
        second.row_ordinal = ordinal;
        let rows = [
            QueryStoreDefinitionRecord { reference: original[0].reference.clone(), definition: original[0].definition },
            QueryStoreDefinitionRecord { reference: second, definition: original[0].definition },
        ];
        assert!(fixture.runtime_report(&rows, 2, true).is_err());
    }
    let mut fixture = captured_fixture();
    let mut equivalent = fixture.definitions[0].clone();
    equivalent.identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T05:00:00+00:00".into());
    fixture.definitions.push(equivalent);
    fixture.seal();
    assert!(fixture.runtime_report(&fixture.rows(), 2, true).is_err());
}

#[test]
fn same_capture_association_preserves_the_bounded_definition_population() {
    let mut fixture = captured_fixture();
    let prototype = fixture.definitions[0].clone();
    fixture.definitions = (0..=MAX_DEFINITION_RECORDS)
        .map(|index| {
            let mut definition = prototype.clone();
            definition.identity.query_id = (42 + index).to_string();
            definition
        })
        .collect();
    fixture.seal();
    let rows = fixture.rows();
    let bounded = fixture.runtime_report(&rows[..MAX_DEFINITION_RECORDS], MAX_DEFINITION_RECORDS as u32, true).unwrap();
    assert_eq!(statement(&bounded).binding, QueryStoreStatementBinding::Captured);
    assert!(fixture.runtime_report(&rows, (MAX_DEFINITION_RECORDS + 1) as u32, true).is_err());
}
