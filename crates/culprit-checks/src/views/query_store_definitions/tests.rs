use super::*;
use culprit_core::{DatabaseIdentity, DatabaseLocation, Engine, SourceOutcome, SourceState};

struct Fixture {
    runtime_source: CollectionDescription,
    definition_source: CollectionDescription,
    compiler_context: NativeReadContext,
    runtime: RuntimeMeasurement,
    compilation: OperationalObservation,
    definitions: Vec<QueryDefinition>,
}
impl Fixture {
    fn new() -> Self {
        let mut database = DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None);
        database.incarnation = Some("original-query-store-database".into());
        let source = CollectionDescription {
            target: database.into(),
            partition: SourcePartition::Target,
            source: "original-runtime-source".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::QueryHistory(QueryHistoryDescription {
                acquisition_owner: None,
                interval: TelemetryInterval {
                    native_id: "1".into(),
                    from: "2020-01-01T06:00:00Z".into(),
                    until: "2020-01-01T07:00:00Z".into(),
                },
                population_catalog: None,
                population_binding: QueryHistoryPopulationBinding::Unknown,
                waits: None,
                dimensions: Default::default(),
                native_properties: [(
                    PageServerReadAvailability::PROPERTY.into(),
                    PageServerReadAvailability::Available.native_property().into(),
                )]
                .into(),
            }),
        };
        let identity = NativeQueryIdentity {
            query_id: "42".into(),
            qualifiers: [
                ("context_settings_id".into(), "5".into()),
                ("object_id".into(), "0".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
            ]
            .into(),
        };
        let native = MssqlQueryCompilation {
            identity: identity.clone(),
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            origin: StatementOrigin::Unknown,
            last_compile_at: "2020-01-01T06:00:00Z".into(),
            compilations: "2".into(),
            average_duration_us: 1000.0,
            timings: Default::default(),
            memory: None,
            seconds_since_first_compile: 3600,
            reset: culprit_core::ResetMarker::Unknown,
        };
        let compilation = OperationalObservation {
            source: "original-compiler-source".into(),
            key: native.key().unwrap(),
            fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
        };
        let runtime = RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new("17")
                    .qualified("plan_id", "4")
                    .qualified("execution_type", "0")
                    .qualified("replica_group_id", "1"),
                references: StatementReferences {
                    query: Some(NativeDefinitionReference {
                        native: identity.native(),
                        definition_id: Some(identity.definition_id(&source).unwrap()),
                    }),
                    plan: None,
                },
                cohort: None,
            },
            executions: "9007199254740993".into(),
            metrics: vec![],
            page_server_reads: Some(Box::new(PageServerReadStatistics {
                availability: PageServerReadAvailability::Available,
                last_pages: Some("9007199254740993".into()),
                minimum_pages: Some("0".into()),
                ..Default::default()
            })),
            first_execution_at: Some("2020-01-01T06:01:00Z".into()),
            last_execution_at: Some("2020-01-01T06:59:00Z".into()),
        };
        let capture = CaptureIdentity {
            observation_id: digest(source.source.as_bytes()),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"original-definition-conformance-outbox"),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let TelemetryRecord::QueryRuntime(runtime) =
            ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(runtime)).unwrap().record
        else {
            panic!()
        };
        let compiler_context = NativeReadContext {
            target: source.target.clone(),
            partition: SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        };
        let mut definition_source = source.clone();
        definition_source.source = "independent-original-definitions".into();
        let SourceObservation::QueryHistory(history) = &mut definition_source.observation else { panic!() };
        history.interval = TelemetryInterval {
            native_id: "2".into(),
            from: "2020-01-01T08:00:00Z".into(),
            until: "2020-01-01T09:00:00Z".into(),
        };
        let definition = QueryDefinition {
            id: identity.definition_id(&definition_source).unwrap(),
            identity,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            variant_family: None,
            context: Default::default(),
            context_available: false,
            source_text_bytes: None,
            sql: DefinitionPayload::sql("SELECT id FROM dbo.orders WHERE tenant_id=81724", Engine::Mssql),
        };
        Self {
            runtime_source: source,
            definition_source,
            compiler_context,
            runtime,
            compilation,
            definitions: vec![definition],
        }
    }
    fn seal(&mut self) {
        for row in &mut self.definitions {
            row.id = row.identity.definition_id(&self.definition_source).unwrap();
        }
    }
    fn reference(source: &CollectionDescription, record: TelemetryRecord, ordinal: u32) -> DiagnosticEvidenceRef {
        let capture = CaptureIdentity {
            observation_id: digest(source.source.as_bytes()),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"original-definition-conformance-outbox"),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let exported = ExportedRecord::new(source, &capture, record.clone()).unwrap();
        assert_eq!(exported.record, record, "fixtures must be canonical before receipt construction");
        DiagnosticEvidenceRef::from_export(source, &digest(source.source.as_bytes()), &exported, ordinal).unwrap()
    }
    fn rows(&self) -> Vec<QueryStoreDefinitionRecord<'_>> {
        self.definitions
            .iter()
            .enumerate()
            .map(|(ordinal, definition)| QueryStoreDefinitionRecord {
                reference: Self::reference(
                    &self.definition_source,
                    TelemetryRecord::QueryDefinition(definition.clone()),
                    ordinal as u32,
                ),
                definition,
            })
            .collect()
    }
    fn evidence<'a>(
        &'a self,
        rows: &'a [QueryStoreDefinitionRecord<'a>],
        selected: u32,
        complete: bool,
    ) -> QueryStoreDefinitionEvidence<'a> {
        QueryStoreDefinitionEvidence {
            source: &self.definition_source,
            rows,
            selected_rows: selected,
            read_complete: complete,
        }
    }
    fn runtime_report(
        &self,
        rows: &[QueryStoreDefinitionRecord<'_>],
        selected: u32,
        complete: bool,
    ) -> Result<culprit_core::page_server_read_report::PageServerReadReport> {
        let capture = CaptureIdentity {
            observation_id: digest(self.runtime_source.source.as_bytes()),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"original-definition-conformance-outbox"),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let read = CapturedReadRef::new(
            &self.runtime_source,
            capture,
            digest(self.runtime_source.source.as_bytes()),
            vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: 1 }],
        )
        .unwrap();
        let exported = ExportedRecord::new(
            &self.runtime_source,
            &read.capture,
            TelemetryRecord::QueryRuntime(self.runtime.clone()),
        )
        .unwrap();
        let reference = CapturedRecordReference::from_export(&read, &self.runtime_source, &exported, 0).unwrap();
        let query = self.runtime.key.references.query.as_ref().is_some_and(NativeDefinitionReference::is_resolved);
        let plan = self.runtime.key.references.plan.as_ref();
        let coverage = ReferenceCoverage {
            queries: DefinitionCoverage { required: 1, resolved: u32::from(query), unresolved: u32::from(!query) },
            plans: DefinitionCoverage {
                required: u32::from(plan.is_some()),
                resolved: u32::from(plan.is_some_and(NativeDefinitionReference::is_resolved)),
                unresolved: u32::from(plan.is_some_and(|plan| !plan.is_resolved())),
            },
            complete: query && plan.is_none_or(NativeDefinitionReference::is_resolved),
            integrity_complete: true,
            ..Default::default()
        };
        crate::page_server_reads::explain_archive(crate::page_server_reads::PageServerReadEvidence {
            source: &self.runtime_source,
            read: &read,
            reference_coverage: &coverage,
            rows: &[crate::page_server_reads::RuntimeEvidence { reference, runtime: &self.runtime }],
            selected_rows: 1,
            read_complete: true,
            definitions: Some(self.evidence(rows, selected, complete)),
        })
    }
    fn compiler_report(
        &self,
        rows: &[QueryStoreDefinitionRecord<'_>],
        selected: u32,
        complete: bool,
    ) -> Result<culprit_core::CompilationReport> {
        let outcome = SourceOutcome {
            source: self.compilation.source.clone(),
            state: SourceState::Collected,
            reason: None,
            reason_message: None,
        };
        crate::compilation::explain(crate::compilation::CompilationEvidence {
            context: Some(&self.compiler_context),
            observations: &[&self.compilation],
            source: Some(&outcome),
            definitions: Some(self.evidence(rows, selected, complete)),
        })
    }
    fn assert_binding(&self, expected: QueryStoreStatementBinding) {
        let rows = self.rows();
        let runtime = self.runtime_report(&rows, rows.len() as u32, true).unwrap();
        let compilation = self.compiler_report(&rows, rows.len() as u32, true).unwrap();
        for statement in
            [runtime.entries[0].statement.as_ref().unwrap(), compilation.entries[0].statement.as_ref().unwrap()]
        {
            assert_eq!(statement.binding, expected);
            if expected != QueryStoreStatementBinding::Matched {
                assert!(statement.preview.is_none());
            }
        }
    }
}

#[test]
fn both_domains_use_one_original_definition_without_rewriting_native_facts_or_transferring_clocks() {
    let fixture = Fixture::new();
    let rows = fixture.rows();
    let runtime_before = fixture.runtime.clone();
    let compiler_before = fixture.compilation.clone();
    let runtime = fixture.runtime_report(&rows, 1, true).unwrap();
    let compilation = fixture.compiler_report(&rows, 1, true).unwrap();
    let current = runtime.entries[0].statement.as_ref().unwrap();
    let compiler = compilation.entries[0].statement.as_ref().unwrap();
    assert_eq!(current.binding, QueryStoreStatementBinding::Matched);
    assert_eq!(current.definition, compiler.definition);
    assert_eq!(current.origin, StatementOrigin::Application);
    assert_eq!(current.preview, compiler.preview);
    assert!(!current.preview.as_ref().unwrap().contains("81724"));
    assert_eq!(runtime.entries[0].executions, "9007199254740993");
    assert!(
        runtime.entries[0].explanation.metrics.iter().any(|metric| metric.text.as_deref() == Some("9007199254740993"))
    );
    assert_eq!(fixture.runtime, runtime_before);
    assert_eq!(fixture.compilation, compiler_before);
    assert_ne!(
        fixture.runtime_source.query_history().unwrap().interval,
        fixture.definition_source.query_history().unwrap().interval
    );
    assert_ne!(
        fixture.compiler_context.observed_from,
        fixture.definition_source.query_history().unwrap().interval.from
    );
    assert_ne!(
        fixture.runtime.key.references.query.as_ref().unwrap().definition_id.as_ref(),
        Some(&fixture.definitions[0].id),
        "each source owns its own definition id"
    );
    assert!(!current.explanation.contains("compiler"), "runtime never borrows compiler-specific wording");
}

#[test]
fn no_native_incarnation_component_can_be_replaced_by_a_number_hash_text_or_single_candidate() {
    for field in ["query_id", "context_settings_id", "object_id", "query_hash", "initial_compile_at", "extra"] {
        let mut fixture = Fixture::new();
        let definition = &mut fixture.definitions[0];
        match field {
            "query_id" => definition.identity.query_id = "43".into(),
            "context_settings_id" => {
                definition.identity.qualifiers.insert(field.into(), "6".into());
            }
            "object_id" => {
                definition.identity.qualifiers.insert(field.into(), "2".into());
            }
            "query_hash" => {
                definition.identity.qualifiers.insert(field.into(), "1123456789ABCDEF".into());
            }
            "initial_compile_at" => {
                definition.identity.qualifiers.insert(field.into(), "2020-01-01T05:00:01Z".into());
            }
            _ => {
                definition.identity.qualifiers.insert(field.into(), "different".into());
            }
        }
        fixture.seal();
        fixture.assert_binding(QueryStoreStatementBinding::NotObserved);
    }
}

#[test]
fn aliases_and_renames_require_the_same_original_native_database_and_unknown_identity_stays_unverified() {
    for (case, binding) in [
        ("alias", QueryStoreStatementBinding::Matched),
        ("rename", QueryStoreStatementBinding::Matched),
        ("unknown", QueryStoreStatementBinding::SourceUnverified),
        ("changed", QueryStoreStatementBinding::SourceChanged),
    ] {
        let mut fixture = Fixture::new();
        let CollectionTarget::Database(database) = &mut fixture.definition_source.target else { panic!() };
        match case {
            "alias" => {
                database.location =
                    DatabaseLocation::Server { host: "another-route.invalid".into(), port: 1433, instance: None }
            }
            "rename" => database.database = "renamed".into(),
            "unknown" => database.incarnation = None,
            _ => database.incarnation = Some("different-native-database".into()),
        }
        fixture.seal();
        fixture.assert_binding(binding);
    }
}

#[test]
fn runtime_identity_absence_retains_page_facts_but_contradictory_present_references_are_invalid() {
    let mut missing = Fixture::new();
    missing.runtime.key.references.query = None;
    let pages = missing.runtime.page_server_reads.clone();
    assert!(runtime_query_identity(&missing.runtime_source, &missing.runtime.key).unwrap().is_none());
    let capture = CaptureIdentity {
        observation_id: digest(b"genuine-unresolved-runtime"),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"genuine-unresolved-outbox"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported =
        ExportedRecord::new(&missing.runtime_source, &capture, TelemetryRecord::QueryRuntime(missing.runtime.clone()))
            .unwrap();
    assert!(exported.record_id.is_none(), "an informational position must not be fabricated as a diagnostic ID");
    assert!(
        DiagnosticEvidenceRef::from_export(
            &missing.runtime_source,
            &digest(b"original-unresolved-manifest"),
            &exported,
            0
        )
        .is_err()
    );
    assert_eq!(missing.runtime.page_server_reads, pages);
    let report = missing.runtime_report(&missing.rows(), 1, true).unwrap();
    assert_eq!(report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::IdentityUnavailable);
    assert_eq!(report.entries[0].executions, "9007199254740993");
    let culprit_core::page_server_read_report::PageServerReadEntryIdentity::RuntimeFragment { reference, .. } =
        &report.entries[0].identity
    else {
        panic!()
    };
    assert!(reference.is_informational());

    for field in ["initial_compile_at", "context_settings_id"] {
        let mut fixture = Fixture::new();
        let reference = fixture.runtime.key.references.query.as_mut().unwrap();
        reference.native.qualifiers.remove(field);
        reference.definition_id = None;
        let rows = fixture.rows();
        let report = fixture.runtime_report(&rows, 1, true).unwrap();
        assert_eq!(
            report.entries[0].statement.as_ref().unwrap().binding,
            QueryStoreStatementBinding::IdentityUnavailable
        );
        assert_eq!(report.entries[0].executions, "9007199254740993");
        assert!(report.read_complete);
    }
    let mut fixture = Fixture::new();
    fixture.runtime.key.references.query.as_mut().unwrap().definition_id =
        Some(digest(b"contradictory original query pointer"));
    assert!(fixture.runtime_report(&fixture.rows(), 1, true).is_err());
    let mut fixture = Fixture::new();
    fixture.runtime.key.references.plan =
        Some(NativeDefinitionReference { native: NativeIdentity::new("5"), definition_id: None });
    assert!(fixture.runtime_report(&fixture.rows(), 1, true).is_err());
}

#[test]
fn restricted_partial_and_missing_sql_are_not_equivalent_to_zero_or_unmeasured_page_activity() {
    let fixture = Fixture::new();
    for complete in [true, false] {
        let report = fixture.runtime_report(&[], u32::from(!complete), complete).unwrap();
        assert_eq!(report.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::NotObserved);
        assert_eq!(report.definition_read.unwrap().read_complete, complete);
        assert!(report.read_complete);
    }
    let mut fixture = Fixture::new();
    fixture.definitions[0].sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let rows = fixture.rows();
    let report = fixture.runtime_report(&rows, 1, true).unwrap();
    let statement = report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.binding, QueryStoreStatementBinding::Matched);
    assert_eq!(statement.unavailable, Some(PayloadUnavailable::Restricted));
    assert!(statement.preview.is_none());
}

#[test]
fn both_consumers_reject_duplicate_receipts_changed_payloads_and_valid_sha_unredacted_replacements() {
    let fixture = Fixture::new();
    let rows = fixture.rows();
    let duplicate = [
        QueryStoreDefinitionRecord { reference: rows[0].reference.clone(), definition: rows[0].definition },
        QueryStoreDefinitionRecord { reference: rows[0].reference.clone(), definition: rows[0].definition },
    ];
    assert!(fixture.runtime_report(&duplicate, 2, true).is_err());
    assert!(fixture.compiler_report(&duplicate, 2, true).is_err());
    let mut changed = fixture.definitions[0].clone();
    let DefinitionPayload::Available { redaction_version, .. } = &changed.sql else { panic!() };
    let text = "SELECT id FROM dbo.orders WHERE tenant_id=81724";
    changed.sql = DefinitionPayload::Available {
        text: text.into(),
        sha256: digest(text.as_bytes()),
        redaction_version: *redaction_version,
    };
    assert!(changed.is_valid());
    let replacement = [QueryStoreDefinitionRecord { reference: rows[0].reference.clone(), definition: &changed }];
    assert!(fixture.runtime_report(&replacement, 1, true).is_err());
    assert!(fixture.compiler_report(&replacement, 1, true).is_err());
}

#[test]
fn origin_and_shortened_verified_sql_remain_separate_from_runtime_causation() {
    let mut fixture = Fixture::new();
    fixture.definitions[0].origin = StatementOrigin::EngineInternal;
    let columns = (0..96).map(|index| format!("retained_context_{index}")).collect::<Vec<_>>().join(", ");
    fixture.definitions[0].sql = DefinitionPayload::sql(&format!("SELECT id,{columns} FROM dbo.orders"), Engine::Mssql);
    let rows = fixture.rows();
    let report = fixture.runtime_report(&rows, 1, true).unwrap();
    let statement = report.entries[0].statement.as_ref().unwrap();
    assert_eq!(statement.origin, StatementOrigin::EngineInternal);
    assert!(statement.preview_truncated);
    assert_eq!(statement.preview.as_ref().unwrap().chars().count(), 512);
    assert!(statement.definition.is_some());
    assert!(report.limitations.iter().any(|text| text == &culprit_core::t!("view.page_server_reads.limit.origin")));
}

#[test]
fn an_empty_companion_still_rejects_foreign_engine_partition_and_malformed_native_history() {
    for case in ["engine", "partition", "clock"] {
        let mut fixture = Fixture::new();
        match case {
            "engine" => {
                let CollectionTarget::Database(database) = &mut fixture.definition_source.target else { panic!() };
                database.engine = Engine::Postgres;
            }
            "partition" => {
                fixture.definition_source.partition = SourcePartition::InstanceLocal { member: None, epoch: None }
            }
            _ => {
                let SourceObservation::QueryHistory(history) = &mut fixture.definition_source.observation else {
                    panic!()
                };
                history.interval.until = "not-a-native-clock".into();
            }
        }
        assert!(fixture.runtime_report(&[], 0, true).is_err(), "{case}");
        assert!(fixture.compiler_report(&[], 0, true).is_err(), "{case}");
    }
}

#[test]
fn incomplete_native_identity_with_a_supplied_conflicting_definition_id_is_integrity_not_missing_sql() {
    let mut fixture = Fixture::new();
    let query = fixture.runtime.key.references.query.as_mut().unwrap();
    query.native.qualifiers.remove("context_settings_id");
    let incomplete =
        NativeQueryIdentity { query_id: query.native.id.clone(), qualifiers: query.native.qualifiers.clone() };
    query.definition_id = Some(incomplete.definition_id(&fixture.runtime_source).unwrap());
    let rows = fixture.rows();
    let unknown = fixture.runtime_report(&rows, 1, true).unwrap();
    assert_eq!(unknown.entries[0].statement.as_ref().unwrap().binding, QueryStoreStatementBinding::IdentityUnavailable);
    assert!(unknown.entries[0].statement.as_ref().unwrap().preview.is_none());
    fixture.runtime.key.references.query.as_mut().unwrap().definition_id =
        Some(digest(b"valid-sha-foreign-definition"));
    let rows = fixture.rows();
    assert!(
        fixture.runtime_report(&rows, 1, true).is_err(),
        "even an incomplete identity cannot suppress a present contradictory source-scoped reference"
    );
}
