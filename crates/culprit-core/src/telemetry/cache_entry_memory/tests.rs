use super::*;

fn statement(start: u32) -> CacheEntryStatement {
    CacheEntryStatement {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: start,
                statement_end_bytes: -1,
                created_without_offset: "2020-01-01T10:00:00.003".into(),
                plan_generation: "2".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: None,
            source_text_complete: false,
        },
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    }
}

fn entry() -> MssqlCacheEntryMemory {
    MssqlCacheEntryMemory {
        observed_at: "2020-01-01T12:00:00Z".into(),
        plan_handle: "0x0102".into(),
        sql_handle: "0x0304".into(),
        size_bytes: "9007199254740993".into(),
        cache_object_type: "Compiled Plan".into(),
        object_type: "Proc".into(),
        lookup_count: "1".into(),
        reference_count: "2".into(),
        cache_keys: vec![CacheKeyAttribute {
            name: "set_options".into(),
            native_type: Some("int".into()),
            value: Some("251".into()),
        }],
        statements: vec![statement(0), statement(20)],
    }
}

fn focus() -> CacheEntryMemoryFocus {
    let mut context = super::super::native_read_context::tests::read_context();
    context.observed_until = "2020-01-01T12:00:00Z".into();
    CacheEntryMemoryFocus {
        context,
        statements: entry().statements.into_iter().map(|statement| statement.context).collect(),
    }
}

#[test]
fn one_whole_entry_keeps_multiple_statements_without_duplicating_or_rounding_its_counters() {
    let original = entry();
    assert!(original.is_valid());
    let recovered: MssqlCacheEntryMemory = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(recovered, original);
    assert_eq!(recovered.statements.len(), 2);
    assert_eq!(recovered.size_bytes, "9007199254740993");
    let mut changed = recovered.clone();
    changed.lookup_count = "200".into();
    changed.reference_count = "4".into();
    changed.size_bytes = "9007199254740994".into();
    assert_eq!(changed.key().unwrap(), recovered.key().unwrap(), "volatile gauges do not create another entry");
    changed.observed_at = "2020-01-01T12:00:01Z".into();
    assert_ne!(changed.key().unwrap(), recovered.key().unwrap(), "another point is not a counter lifetime");
}

#[test]
fn selectors_require_a_qualified_local_owner_and_unique_exact_statement_incarnations() {
    let original = focus();
    assert!(original.is_valid());
    let mut reversed = original.clone();
    reversed.statements.reverse();
    assert_eq!(original.key().unwrap(), reversed.key().unwrap());
    for invalid in [
        CacheEntryMemoryFocus { statements: vec![], ..original.clone() },
        CacheEntryMemoryFocus {
            statements: vec![original.statements[0].clone(), original.statements[0].clone()],
            ..original.clone()
        },
        CacheEntryMemoryFocus {
            context: NativeReadContext { partition: SourcePartition::Target, ..original.context.clone() },
            ..original.clone()
        },
        CacheEntryMemoryFocus {
            context: NativeReadContext {
                partition: SourcePartition::InstanceLocal { member: None, epoch: None },
                ..original.context.clone()
            },
            ..original.clone()
        },
    ] {
        assert!(!invalid.is_valid());
        assert!(invalid.key().is_err());
    }
    let mut changed = original.clone();
    changed.statements[1].database_id += 1;
    assert!(!changed.is_valid());
    changed = original.clone();
    changed.statements[1].identity.sql_handle = "0x0405".into();
    assert!(!changed.is_valid(), "one native plan cannot silently acquire two original batches");
    changed = original.clone();
    changed.statements[0].identity.plan_generation = "3".into();
    assert_ne!(changed.key().unwrap(), original.key().unwrap());
    changed = original.clone();
    changed.statements[0].query_hash = None;
    assert_ne!(changed.key().unwrap(), original.key().unwrap(), "retained original context is immutable");
}

#[test]
fn contradictory_or_duplicate_statement_membership_cannot_charge_an_entry_twice() {
    let mut invalid = entry();
    invalid.statements.push(invalid.statements[0].clone());
    assert!(!invalid.is_valid());
    for field in ["plan", "sql", "database"] {
        let mut invalid = entry();
        match field {
            "plan" => invalid.statements[1].context.identity.plan_handle = "0x0506".into(),
            "sql" => invalid.statements[1].context.identity.sql_handle = "0x0506".into(),
            _ => invalid.statements[1].context.database_id += 1,
        }
        assert!(!invalid.is_valid(), "{field}");
    }
}

#[test]
fn native_key_unknown_values_remain_unknown_and_oversized_or_duplicate_attributes_are_rejected() {
    let mut value = entry();
    value.cache_keys[0].value = None;
    assert!(value.is_valid());
    value.cache_keys[0].value = Some("x".repeat(MAX_CACHE_KEY_VALUE_BYTES + 1));
    assert!(!value.is_valid());
    value = entry();
    value.cache_keys.push(value.cache_keys[0].clone());
    assert!(!value.is_valid());
    value = entry();
    value.lookup_count = "not-native".into();
    assert!(!value.is_valid());
}

#[test]
fn available_sql_requires_the_original_complete_nonzero_handle_context() {
    let mut row = statement(0);
    row.statement = DefinitionPayload::sql("SELECT id FROM dbo.orders", crate::Engine::Mssql);
    assert!(!row.is_valid());
    row.context.source_text_complete = true;
    row.context.source_text_bytes = Some("52".into());
    assert!(row.is_valid());
    row.context.source_text_bytes = Some("0".into());
    assert!(!row.is_valid());
    row.context.source_text_bytes = Some("52".into());
    row.context.identity.sql_handle = "0x0000".into();
    assert!(!row.is_valid());
}

#[test]
fn a_focus_admits_only_original_incarnations_while_new_text_availability_stays_independent() {
    let focus = focus();
    let original = entry();
    assert!(focus.accepts_entry(&original));
    for field in ["generation", "offset", "database", "clock", "hash"] {
        let mut changed = original.clone();
        match field {
            "generation" => changed.statements[0].context.identity.plan_generation = "3".into(),
            "offset" => changed.statements[0].context.identity.statement_start_bytes = 2,
            "database" => changed.statements.iter_mut().for_each(|row| row.context.database_id = 8),
            "clock" => changed.observed_at = "2020-01-01T11:59:59Z".into(),
            _ => changed.statements[0].context.query_hash = Some("0xFEDCBA9876543210".into()),
        }
        assert!(!focus.accepts_entry(&changed), "{field}");
    }
    let mut extra = original.clone();
    extra.statements.push(statement(40));
    assert!(!focus.accepts_entry(&extra), "a related batch does not admit an unselected statement");
    let mut current = original.clone();
    current.statements[0].context.source_text_bytes = Some("52".into());
    current.statements[0].context.source_text_complete = true;
    current.statements[0].statement = DefinitionPayload::sql("SELECT id FROM dbo.orders", crate::Engine::Mssql);
    assert!(focus.accepts_entry(&current));
    assert!(!focus.statements[0].source_text_complete, "new metadata never rewrites original provenance");
    let mut known = focus;
    known.statements[0].source_text_bytes = Some("52".into());
    current.statements[0].context.source_text_bytes = Some("54".into());
    assert!(!known.accepts_entry(&current), "a changed known batch size is not new availability");
}
