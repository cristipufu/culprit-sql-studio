use super::*;

fn payload(text: &str) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: Some((text.encode_utf16().count() * 2).to_string()),
        native_sha256: Some(digest(text.as_bytes())),
        payload: DefinitionPayload::Available {
            text: text.into(),
            sha256: digest(text.as_bytes()),
            redaction_version: REDACTION_VERSION,
        },
    }
}

pub(super) fn retained() -> MssqlRetainedPlanDefinition {
    MssqlRetainedPlanDefinition {
        current_type_schema: None,
        relational_input_proof: None,
        parameter_context_proof: None,
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0200".into(),
                sql_handle: "0x0300".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2026-11-14T12:00:00".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: None,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: None,
            source_text_complete: false,
        },
        statement: payload("SELECT value FROM @Rows WHERE value = 9264"),
        module_parameters: Some(MssqlModuleParameters {
            database_id: 7,
            object_id: 1001,
            module_number: 1,
            sql_handle: "0x0300".into(),
            table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
        }),
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: None,
        database_last_query_plan_stats: Some(true),
        batch_plan: payload(
            r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT value FROM @Rows WHERE value = 9264" /></Statements></Batch></BatchSequence></ShowPlanXML>"#,
        ),
    }
}

#[test]
fn retained_module_metadata_must_match_its_own_original_database_and_sql_handle() {
    let value = retained();
    assert!(value.is_valid());
    for change in ["database", "sql-handle", "placeholder-plan", "invalid-statement"] {
        let mut changed = value.clone();
        match change {
            "database" => changed.module_parameters.as_mut().unwrap().database_id = 8,
            "sql-handle" => changed.module_parameters.as_mut().unwrap().sql_handle = "0x0301".into(),
            "placeholder-plan" => changed.context.identity.plan_handle = "0x0000".into(),
            _ => {
                changed.statement.payload = DefinitionPayload::Available {
                    text: "SELECT 1".into(),
                    sha256: "not-a-digest".into(),
                    redaction_version: REDACTION_VERSION,
                }
            }
        }
        assert!(!changed.is_valid(), "{change}");
    }
    let mut unavailable = value;
    unavailable.statement.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    assert!(unavailable.is_valid(), "known declarations and unreadable statement text stay separate facts");
}

#[test]
fn retained_statement_and_plan_redact_together_and_have_two_exact_payload_references() {
    let mut value = retained();
    let fingerprints = (value.statement.native_sha256.clone(), value.batch_plan.native_sha256.clone());
    value.redact(crate::Engine::Mssql);
    assert!(value.is_valid());
    assert_eq!((value.statement.native_sha256.clone(), value.batch_plan.native_sha256.clone()), fingerprints);
    let original = serde_json::to_value(&value).unwrap();
    assert!(!original.to_string().contains("9264"));
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new(
            "retained-plan-source".into(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(value)),
        )
        .unwrap(),
    );
    let fields = record.definition_payloads().collect::<Vec<_>>();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].pointer, "/record/data/fact/data/statement/payload");
    assert_eq!(fields[1].pointer, "/record/data/fact/data/batch_plan/payload");
    let encoded = serde_json::json!({"record": &record});
    for field in fields {
        assert_eq!(encoded.pointer(field.pointer.as_ref()).unwrap(), &serde_json::to_value(field.payload).unwrap());
    }
}
