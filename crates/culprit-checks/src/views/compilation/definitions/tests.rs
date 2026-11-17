use super::*;
use crate::views::compilation::tests::{native, observation};
use culprit_core::{DatabaseIdentity, DatabaseLocation, Engine, SourceOutcome, SourceState};

struct Fixture {
    context: NativeReadContext,
    source: CollectionDescription,
    definitions: Vec<QueryDefinition>,
}

impl Fixture {
    fn new(row: &OperationalObservation) -> Self {
        let mut database = DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None);
        database.incarnation = Some("native-query-store-database".into());
        let context = NativeReadContext {
            target: database.into(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        };
        let source = CollectionDescription {
            target: context.target.clone(),
            partition: SourcePartition::Target,
            source: "original-query-definitions".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::QueryHistory(QueryHistoryDescription {
                // A definition's runtime interval supplies no compiler acquisition clock or execution cohort.
                interval: TelemetryInterval {
                    native_id: "independent-native-runtime-period".into(),
                    from: "2019-01-01T00:00:00Z".into(),
                    until: "2019-01-01T01:00:00Z".into(),
                },
                population_catalog: None,
                population_binding: QueryHistoryPopulationBinding::Unknown,
                waits: None,
                dimensions: Default::default(),
                native_properties: Default::default(),
            }),
        };
        let OperationalFact::MssqlQueryCompilation(native) = &row.fact else { panic!() };
        let sql = "SELECT id FROM dbo.original_compile_input WHERE tenant_id=81724 OPTION(RECOMPILE)";
        let mut fixture = Self {
            context,
            source,
            definitions: vec![QueryDefinition {
                id: String::new(),
                identity: native.identity.clone(),
                origin: StatementOrigin::Application,
                monitoring_template_id: None,
                variant_family: None,
                context: [("set_options".into(), "00000000000000FB".into())].into(),
                context_available: true,
                source_text_bytes: Some((sql.encode_utf16().count() * 2).to_string()),
                sql: DefinitionPayload::sql(sql, Engine::Mssql),
            }],
        };
        fixture.seal();
        fixture
    }

    fn seal(&mut self) {
        for definition in &mut self.definitions {
            definition.id = definition.identity.definition_id(&self.source).unwrap();
        }
    }

    fn rows(&self) -> Vec<CompilationDefinitionRecord<'_>> {
        let capture = CaptureIdentity {
            observation_id: "original-compiler-definition-capture".into(),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"owned-compiler-definition-outbox"),
                owner_epoch: "1".into(),
                sequence: "3".into(),
            }),
        };
        self.definitions
            .iter()
            .enumerate()
            .map(|(ordinal, definition)| {
                let record = TelemetryRecord::QueryDefinition(definition.clone());
                let exported = ExportedRecord::new(&self.source, &capture, record.clone()).unwrap();
                assert_eq!(exported.record, record, "the original fixture must already be canonical");
                CompilationDefinitionRecord {
                    reference: DiagnosticEvidenceRef::from_export(
                        &self.source,
                        &digest(b"original-compiler-definition-manifest"),
                        &exported,
                        ordinal as u32,
                    )
                    .unwrap(),
                    definition,
                }
            })
            .collect()
    }

    fn explain(&self, row: &OperationalObservation) -> CompilationReport {
        let definitions = self.rows();
        explained(row, Some(&self.context), &self.source, &definitions, definitions.len() as u32, true).unwrap()
    }
}

fn expensive() -> OperationalObservation {
    let mut row = observation("12");
    native(&mut row).average_duration_us = 1_000_000.0;
    row
}

fn explained(
    row: &OperationalObservation,
    context: Option<&NativeReadContext>,
    source: &CollectionDescription,
    definitions: &[CompilationDefinitionRecord<'_>],
    selected_rows: u32,
    read_complete: bool,
) -> Result<CompilationReport> {
    let outcome =
        SourceOutcome { source: row.source.clone(), state: SourceState::Collected, reason: None, reason_message: None };
    super::super::explain(CompilationEvidence {
        context,
        observations: &[row],
        source: Some(&outcome),
        definitions: Some(CompilationDefinitionEvidence { source, rows: definitions, selected_rows, read_complete }),
    })
}

#[test]
fn an_exact_original_definition_supplies_policy_without_rewriting_counter_origin_or_borrowing_runtime_clocks() {
    let row = expensive();
    let before = row.clone();
    let fixture = Fixture::new(&row);
    let expected_ref = fixture.rows()[0].reference.clone();
    let report = fixture.explain(&row);
    let entry = &report.entries[0];
    let statement = entry.statement.as_ref().unwrap();
    assert_eq!(statement.binding, CompilationStatementBinding::Matched);
    assert_eq!(statement.definition.as_ref(), Some(&expected_ref));
    assert_eq!(statement.origin, StatementOrigin::Application);
    assert!(statement.preview.as_ref().unwrap().contains("OPTION(RECOMPILE)"));
    assert!(!statement.preview.as_ref().unwrap().contains("81724"));
    assert!(entry.title.contains("OPTION(RECOMPILE)"));
    assert!(entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration")));
    let selection = report.definition_read.unwrap();
    assert_eq!(selection.namespace, CompilationDefinitionNamespace::Matched);
    assert_eq!((selection.selected_rows, selection.observed_rows), (1, 1));
    assert!(selection.read_complete);
    assert_eq!(row, before);
    let OperationalFact::MssqlQueryCompilation(native) = &row.fact else { panic!() };
    assert_eq!(native.origin, StatementOrigin::Unknown);
    assert_eq!(native.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
}

#[test]
fn the_same_query_number_hash_or_sql_cannot_replace_any_native_incarnation_field() {
    for (name, value) in [
        ("query_id", "43"),
        ("context_settings_id", "6"),
        ("object_id", "2000"),
        ("query_hash", "1123456789ABCDEF"),
        ("initial_compile_at", "2020-01-01T06:00:01Z"),
    ] {
        let row = expensive();
        let mut fixture = Fixture::new(&row);
        if name == "query_id" {
            fixture.definitions[0].identity.query_id = value.into();
        } else {
            fixture.definitions[0].identity.qualifiers.insert(name.into(), value.into());
        }
        fixture.seal();
        let report = fixture.explain(&row);
        assert_eq!(
            report.entries[0].statement.as_ref().unwrap().binding,
            CompilationStatementBinding::NotObserved,
            "{name}"
        );
        assert!(report.entries[0].statement.as_ref().unwrap().preview.is_none());
        assert!(!report.entries[0].metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration")));
    }
}

#[test]
fn only_native_database_identity_accepts_an_endpoint_alias_and_unknown_or_changed_namespaces_withhold_sql() {
    for (case, expected) in [
        ("alias", CompilationStatementBinding::Matched),
        ("renamed", CompilationStatementBinding::Matched),
        ("unknown", CompilationStatementBinding::SourceUnverified),
        ("incarnation", CompilationStatementBinding::SourceChanged),
        ("database", CompilationStatementBinding::SourceUnverified),
    ] {
        let row = expensive();
        let mut fixture = Fixture::new(&row);
        let database = match &mut fixture.source.target {
            CollectionTarget::Database(database) => database,
            _ => unreachable!(),
        };
        match case {
            "alias" => {
                database.location =
                    DatabaseLocation::Server { host: "another-route.invalid".into(), port: 1433, instance: None }
            }
            "unknown" => database.incarnation = None,
            "renamed" => database.database = "renamed_database".into(),
            "incarnation" => database.incarnation = Some("another-native-database".into()),
            "database" => {
                database.incarnation = None;
                database.database = "another_database".into();
            }
            _ => unreachable!(),
        }
        fixture.seal();
        let report = fixture.explain(&row);
        assert_eq!(report.entries[0].statement.as_ref().unwrap().binding, expected, "{case}");
        if expected != CompilationStatementBinding::Matched {
            assert!(report.entries[0].statement.as_ref().unwrap().preview.is_none());
        }
    }
}

#[test]
fn an_empty_definition_read_still_requires_a_valid_original_source_header() {
    let row = expensive();
    let fixture = Fixture::new(&row);
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &[], 0, true).is_ok());
    for field in ["population", "clock", "native-id"] {
        let mut source = fixture.source.clone();
        match field {
            "population" => {
                let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
                history.population_binding = QueryHistoryPopulationBinding::Unpartitioned;
            }
            "clock" => {
                let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
                history.interval.until = "not-a-native-clock".into();
            }
            "native-id" => {
                let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
                history.interval.native_id.clear();
            }
            _ => unreachable!(),
        }
        assert!(explained(&row, Some(&fixture.context), &source, &[], 0, true).is_err(), "{field}");
    }
}

#[test]
fn warehouse_gaps_and_recorded_payload_restrictions_remain_distinct_from_zero_or_absence() {
    let row = expensive();
    let fixture = Fixture::new(&row);
    let partial = explained(&row, Some(&fixture.context), &fixture.source, &[], 1, false).unwrap();
    assert_eq!(partial.entries[0].statement.as_ref().unwrap().binding, CompilationStatementBinding::NotObserved);
    assert!(partial.limitations.contains(&t!("view.compilation.statement.limit.partial")));
    assert!(!partial.definition_read.unwrap().read_complete);
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &[], 1, true).is_err());
    let mut restricted = Fixture::new(&row);
    restricted.definitions[0].sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let report = restricted.explain(&row);
    let statement = report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, CompilationStatementBinding::Matched);
    assert_eq!(statement.unavailable, Some(PayloadUnavailable::Restricted));
    assert!(statement.preview.is_none());
    assert!(report.limitations.contains(&t!("view.compilation.limit.statement")));
}

#[test]
fn receipt_body_tampering_and_conflicting_original_complete_sql_are_integrity_failures() {
    let mut row = expensive();
    let fixture = Fixture::new(&row);
    let rows = fixture.rows();
    let mut changed = fixture.definitions[0].clone();
    changed.sql = DefinitionPayload::sql("SELECT unrelated FROM dbo.other_table OPTION(RECOMPILE)", Engine::Mssql);
    let tampered = [CompilationDefinitionRecord { reference: rows[0].reference.clone(), definition: &changed }];
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &tampered, 1, true).is_err());
    native(&mut row).statement = changed.sql;
    native(&mut row).origin = StatementOrigin::Application;
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &rows, 1, true).is_err());
}

#[test]
fn a_valid_noncanonical_body_cannot_be_repaired_into_a_redacted_receipt_then_rendered_unredacted() {
    let row = expensive();
    let fixture = Fixture::new(&row);
    let rows = fixture.rows();
    let mut noncanonical = fixture.definitions[0].clone();
    let DefinitionPayload::Available { redaction_version, .. } = &noncanonical.sql else { panic!() };
    let text = "SELECT id FROM dbo.original_compile_input WHERE tenant_id=81724 OPTION(RECOMPILE)";
    noncanonical.sql = DefinitionPayload::Available {
        text: text.into(),
        sha256: digest(text.as_bytes()),
        redaction_version: *redaction_version,
    };
    assert!(noncanonical.is_valid());
    let record = TelemetryRecord::QueryDefinition(noncanonical.clone());
    let canonical = ExportedRecord::new(&fixture.source, &rows[0].reference.capture, record.clone()).unwrap();
    assert_ne!(canonical.record, record);
    assert_eq!(
        DiagnosticEvidenceRef::from_export(&fixture.source, &rows[0].reference.manifest_sha256, &canonical, 0).unwrap(),
        rows[0].reference,
        "the attack retains the original canonical receipt"
    );
    let supplied = [CompilationDefinitionRecord { reference: rows[0].reference.clone(), definition: &noncanonical }];
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &supplied, 1, true).is_err());
}

#[test]
fn one_definition_selection_cannot_merge_different_original_captures_or_reuse_a_manifest_ordinal() {
    let row = expensive();
    let mut fixture = Fixture::new(&row);
    let mut second = fixture.definitions[0].clone();
    second.identity.query_id = "43".into();
    fixture.definitions.push(second);
    fixture.seal();
    let mut rows = fixture.rows();
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &rows, 2, true).is_ok());
    rows[1].reference.row_ordinal = 0;
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &rows, 2, true).is_err());
    let mut rows = fixture.rows();
    let mut another = rows[1].reference.capture.clone();
    another.observation_id = "another-original-definition-capture".into();
    another.revision.as_mut().unwrap().sequence = "4".into();
    let exported = ExportedRecord::new(
        &fixture.source,
        &another,
        TelemetryRecord::QueryDefinition(fixture.definitions[1].clone()),
    )
    .unwrap();
    rows[1].reference = DiagnosticEvidenceRef::from_export(
        &fixture.source,
        &digest(b"another-original-definition-manifest"),
        &exported,
        1,
    )
    .unwrap();
    assert!(explained(&row, Some(&fixture.context), &fixture.source, &rows, 2, true).is_err());
}

#[test]
fn matched_culprit_internal_and_unknown_origins_remain_visible_without_application_policy_attribution() {
    for origin in [
        StatementOrigin::CulpritMonitoring,
        StatementOrigin::Culprit,
        StatementOrigin::EngineInternal,
        StatementOrigin::Unknown,
    ] {
        let row = expensive();
        let mut fixture = Fixture::new(&row);
        fixture.definitions[0].origin = origin;
        fixture.definitions[0].monitoring_template_id =
            (origin == StatementOrigin::CulpritMonitoring).then(|| digest(b"original-monitor-template"));
        let report = fixture.explain(&row);
        let entry = &report.entries[0];
        let statement = entry.statement.as_ref().unwrap();
        assert_eq!(statement.origin, origin);
        assert_eq!(statement.monitoring_template_id, fixture.definitions[0].monitoring_template_id);
        assert!(entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.count")));
        assert!(!entry.metrics.iter().any(|metric| metric.is("view.compilation.metric.lifetime_duration")));
    }
}
