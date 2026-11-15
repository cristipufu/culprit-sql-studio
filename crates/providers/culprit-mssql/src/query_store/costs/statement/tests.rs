use super::*;

fn definition() -> Definition {
    let native = Compilation::decode(&crate::query_store::tests::compilation_row()).unwrap();
    Definition {
        id: native.identity.query_id.clone(),
        query: Some(native.identity),
        plan: None,
        origin: StatementOrigin::Application,
        variant_family: None,
        object_name: None,
        payload: Payload::Text("SELECT id FROM dbo.orders WHERE id=987654321 OPTION (RECOMPILE)".into()),
    }
}

#[test]
fn qualified_complete_definition_is_redacted_bounded_and_bound_by_full_native_incarnation() {
    let definition = definition();
    let statement = CompilationStatement::from_definition(&definition).unwrap();
    let DefinitionPayload::Available { text, .. } = &statement.statement else { panic!() };
    assert!(!text.contains("987654321"));
    assert!(text.contains("OPTION (RECOMPILE)"));
    let mut native = Compilation::decode(&crate::query_store::tests::compilation_row()).unwrap();
    assert!(bind_statement(&mut native, [Some(&statement), Some(&statement)]));
    assert_eq!(native.observation().statement, statement.statement);
    assert_eq!(native.observation().origin, StatementOrigin::Application);
    for qualifier in ["context_settings_id", "object_id", "query_hash", "initial_compile_at"] {
        let mut changed = statement.clone();
        changed.identity.qualifiers.insert(qualifier.into(), "another-incarnation".into());
        assert!(!bind_statement(&mut native, [Some(&changed)]));
        assert!(!native.observation().statement.is_available());
        assert_eq!(native.observation().compilations, "9007199254740993");
    }
}

#[test]
fn missing_optional_definition_and_conflicting_duplicates_never_choose_a_representative() {
    let statement = CompilationStatement::from_definition(&definition()).unwrap();
    let mut native = Compilation::decode(&crate::query_store::tests::compilation_row()).unwrap();
    assert!(bind_statement(&mut native, [Some(&statement), None]));
    assert!(!native.statement.is_available());
    let mut conflicting = statement.clone();
    conflicting.statement =
        DefinitionPayload::sql("SELECT id FROM dbo.changed OPTION (RECOMPILE)", culprit_core::Engine::Mssql);
    assert!(!bind_statement(&mut native, [Some(&statement), Some(&conflicting)]));
    assert_eq!(native.origin, StatementOrigin::Unknown);
}

#[test]
fn oversized_or_unavailable_definitions_keep_their_explicit_unknown_policy() {
    let mut definition = definition();
    definition.payload =
        Payload::Text(format!("SELECT {} OPTION (RECOMPILE)", "x".repeat(MAX_RETAINED_STATEMENT_BYTES)));
    let statement = CompilationStatement::from_definition(&definition).unwrap();
    assert_eq!(statement.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized });
    for reason in [PayloadUnavailable::Missing, PayloadUnavailable::Restricted, PayloadUnavailable::Encrypted] {
        definition.payload = Payload::Unavailable(reason);
        let statement = CompilationStatement::from_definition(&definition).unwrap();
        assert_eq!(statement.statement, DefinitionPayload::Unavailable { reason });
    }
    definition.query.as_mut().unwrap().query_id = "44".into();
    assert!(CompilationStatement::from_definition(&definition).is_none());
}
