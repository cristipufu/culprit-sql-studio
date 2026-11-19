use super::*;
use culprit_core::{Engine, telemetry::*};

mod set_options;

fn context() -> NativeReadContext {
    let read = crate::statement_grant_fixture::read(&crate::waits::snapshot_with(Engine::Mssql));
    NativeReadContext::from_snapshot(&read.description).unwrap()
}

fn observation(plan: &str, options: Option<&str>) -> OperationalObservation {
    let read = crate::statement_grant_fixture::read(&crate::waits::snapshot_with(Engine::Mssql));
    let mut statement = read.observations[0].context.clone();
    statement.identity.plan_handle = plan.into();
    statement.identity.sql_handle = "0x0304".into();
    statement.identity.statement_start_bytes = 0;
    statement.identity.statement_end_bytes = 14;
    statement.source_text_bytes = Some("34".into());
    statement.source_text_complete = true;
    let mut second = statement.clone();
    second.identity.statement_start_bytes = 18;
    second.identity.statement_end_bytes = -1;
    let entry = MssqlCacheEntryMemory {
        observed_at: context().observed_from,
        plan_handle: plan.into(),
        sql_handle: "0x0304".into(),
        size_bytes: "9007199254740993".into(),
        cache_object_type: "Compiled Plan".into(),
        object_type: "Proc".into(),
        lookup_count: "1".into(),
        reference_count: "2".into(),
        cache_keys: vec![CacheKeyAttribute {
            name: "set_options".into(),
            native_type: Some("int".into()),
            value: options.map(str::to_owned),
        }],
        statements: vec![
            CacheEntryStatement { context: statement, statement: DefinitionPayload::sql("SELECT 1", Engine::Mssql) },
            CacheEntryStatement { context: second, statement: DefinitionPayload::sql("SELECT 2", Engine::Mssql) },
        ],
    };
    OperationalObservation {
        source: "selected-cache-memory".into(),
        key: entry.key().unwrap(),
        fact: OperationalFact::MssqlCacheEntryMemory(Box::new(entry)),
    }
}

fn native(row: &mut OperationalObservation) -> &mut MssqlCacheEntryMemory {
    let OperationalFact::MssqlCacheEntryMemory(entry) = &mut row.fact else { panic!() };
    entry
}

#[test]
fn a_multi_statement_entry_has_one_exact_footprint_and_lookup_count_without_borrowed_runtime() {
    let row = observation("0x0102", Some("251"));
    let report = explain(&context(), &[&row], None).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].observation_key, row.key);
    let bytes = report.entries[0]
        .metrics
        .iter()
        .filter(|metric| metric.is("view.cache_entry_memory.metric.bytes"))
        .collect::<Vec<_>>();
    assert_eq!(bytes.len(), 1);
    assert_eq!(bytes[0].text.as_deref(), Some("9007199254740993"));
    assert_eq!(
        report.entries[0]
            .metrics
            .iter()
            .find(|metric| metric.is("view.cache_entry_memory.metric.lookups"))
            .unwrap()
            .text
            .as_deref(),
        Some("1")
    );
    assert!(report.entries[0].explanation.contains("whole cache entry"));
    assert!(!report.entries[0].investigation.is_empty());
    let encoded = serde_json::to_value(report).unwrap();
    assert!(encoded.get("workload").is_none());
    assert!(encoded["entries"][0].get("statement").is_none());
    assert!(encoded["entries"][0].get("fix_sql").is_none());
}

#[test]
fn known_native_cache_key_differences_explain_distinct_contexts_without_declaring_waste() {
    let left = observation("0x0102", Some("251"));
    let right = observation("0x0506", Some("252"));
    let report = explain(&context(), &[&right, &left], None).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.context_differences.len(), 1);
    assert_eq!(report.context_differences[0].attributes, ["set_options"]);
    assert!(report.context_differences[0].explanation.contains("does not establish"));
    let refs =
        [&report.context_differences[0].left_observation_key, &report.context_differences[0].right_observation_key];
    assert!(refs.contains(&&left.key) && refs.contains(&&right.key));
}

#[test]
fn a_hash_match_unknown_native_key_or_unknown_owner_never_establishes_the_same_cache_context() {
    let left = observation("0x0102", Some("251"));
    let mut unrelated = observation("0x0506", Some("252"));
    let entry = native(&mut unrelated);
    entry.sql_handle = "0x0708".into();
    for statement in &mut entry.statements {
        statement.context.identity.sql_handle = "0x0708".into();
    }
    unrelated.key = native(&mut unrelated).key().unwrap();
    assert!(explain(&context(), &[&left, &unrelated], None).unwrap().context_differences.is_empty());
    let unknown = observation("0x0506", None);
    let report = explain(&context(), &[&left, &unknown], None).unwrap();
    assert!(report.context_differences.is_empty());
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.keys")));
    let mut unqualified = context();
    unqualified.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let report = explain(&unqualified, &[&left, &observation("0x0506", Some("252"))], None).unwrap();
    assert_eq!(report.entries.len(), 2, "raw points remain visible");
    assert!(report.context_differences.is_empty());
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.owner")));
}

#[test]
fn prepared_and_stub_entries_do_not_turn_one_lookup_into_one_execution_or_a_compile_count() {
    for (cache_type, object_type, expected) in [
        ("Compiled Plan", "Prepared", t!("view.cache_entry_memory.entry.prepared")),
        ("Compiled Plan Stub", "Adhoc", t!("view.cache_entry_memory.entry.stub")),
    ] {
        let mut row = observation("0x0102", Some("251"));
        let entry = native(&mut row);
        entry.cache_object_type = cache_type.into();
        entry.object_type = object_type.into();
        let report = explain(&context(), &[&row], None).unwrap();
        assert!(report.entries[0].explanation.starts_with(&expected));
        assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.lookups")));
    }
}

#[test]
fn clocks_source_mismatches_and_repeated_entries_are_rejected() {
    let row = observation("0x0102", Some("251"));
    assert!(explain(&context(), &[&row, &row], None).is_err());
    let mut wrong_clock = row.clone();
    native(&mut wrong_clock).observed_at = "1900-01-01T00:00:00Z".into();
    wrong_clock.key = native(&mut wrong_clock).key().unwrap();
    assert!(explain(&context(), &[&wrong_clock], None).is_err());
    let source = SourceOutcome {
        source: "different-native-source".into(),
        state: SourceState::Partial,
        reason: None,
        reason_message: None,
    };
    assert!(explain(&context(), &[&row], Some(&source)).is_err());
    let mut target = context();
    target.partition = SourcePartition::Target;
    assert!(explain(&target, &[&row], None).is_err());
}

#[test]
fn unavailable_or_empty_selected_reads_never_claim_a_healthy_empty_cache() {
    let source = SourceOutcome {
        source: "selected-cache-memory".into(),
        state: SourceState::Unavailable,
        reason: Some("native metadata denied".into()),
        reason_message: None,
    };
    let report = explain(&context(), &[], Some(&source)).unwrap();
    assert!(report.entries.is_empty());
    assert_eq!(report.source_reason.as_deref(), Some("native metadata denied"));
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.source_unavailable")));
    assert!(report.summary.contains("does not show that the cache is empty"));
}
