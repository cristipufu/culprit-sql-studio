use super::*;
use crate::telemetry::native_read_context::tests::read_context;

fn query() -> NativeQueryIdentity {
    NativeQueryIdentity {
        query_id: "7".into(),
        qualifiers: [
            ("context_settings_id".into(), "5".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2019-01-01T06:00:00.000000000Z".into()),
        ]
        .into(),
    }
}

fn plan() -> NativePlanIdentity {
    NativePlanIdentity {
        plan_id: "9".into(),
        query: query(),
        qualifiers: [
            ("plan_hash".into(), "0011223344556677".into()),
            ("initial_compile_at".into(), "2019-01-02T06:00:00.000000000Z".into()),
        ]
        .into(),
    }
}

fn request() -> RetainedActualPlanRequest {
    RetainedActualPlanRequest {
        query_id: "7".into(),
        plan_id: Some("9".into()),
        expected_target: Some(read_context().target),
        expected_query: Some(query()),
        expected_plan: Some(plan()),
    }
}

fn provenance() -> RetainedPlanProvenance {
    RetainedPlanProvenance {
        read: read_context(),
        statement: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2019-01-02T08:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: None,
            source_text_complete: false,
        },
        query: Some(query()),
        native_plan: Some(plan()),
    }
}

#[test]
fn independent_selectors_and_exact_optional_query_plan_expectations_are_valid() {
    let original = request();
    assert!(original.validate().is_ok());
    let recovered: RetainedActualPlanRequest = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(recovered, original);
    let independent = RetainedActualPlanRequest {
        query_id: "opaque-query-id".into(),
        plan_id: None,
        expected_target: None,
        expected_query: None,
        expected_plan: None,
    };
    assert!(independent.is_valid(), "the shared request has no engine-specific numeric ID assumption");
    let mut embedded_parent = original;
    embedded_parent.expected_query = None;
    assert!(embedded_parent.is_valid(), "the exact plan expectation already carries its full parent identity");
}

#[test]
fn contradictory_query_plan_and_parent_expectations_fail_before_provider_admission() {
    let original = request();
    let mut changed = original.clone();
    changed.expected_query.as_mut().unwrap().query_id = "8".into();
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.plan_id = Some("10".into());
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.plan_id = None;
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.expected_plan.as_mut().unwrap().query.query_id = "8".into();
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.expected_plan.as_mut().unwrap().query.qualifiers.insert("context_settings_id".into(), "6".into());
    assert!(changed.validate().is_err(), "equal numeric IDs do not erase contradictory parent qualifiers");
    changed = original.clone();
    changed.query_id = " ".into();
    assert!(changed.validate().is_err());
    changed = original;
    changed.plan_id = Some(String::new());
    assert!(changed.validate().is_err());
}

#[test]
fn shared_or_invalid_expected_targets_never_authorize_a_database_plan_lookup() {
    let mut selected = request();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = read_context().partition else { panic!() };
    let shared = CollectionTarget::Shared(member);
    assert!(shared.is_valid());
    selected.expected_target = Some(shared);
    assert!(selected.validate().is_err());
    let CollectionTarget::Database(mut database) = read_context().target else { panic!() };
    database.database.clear();
    selected.expected_target = Some(database.into());
    assert!(selected.validate().is_err());
}

#[test]
fn provenance_accepts_full_optional_identities_and_explicitly_unknown_current_owner() {
    let original = provenance();
    assert!(original.is_valid());
    let recovered: RetainedPlanProvenance = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(recovered, original);
    let mut statement_only = original.clone();
    statement_only.query = None;
    statement_only.native_plan = None;
    statement_only.read.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    statement_only.read.database_writable = None;
    assert!(statement_only.is_valid());
    assert!(!statement_only.read.has_qualified_local_owner());
    let mut query_only = original;
    query_only.native_plan = None;
    assert!(query_only.is_valid());
}

#[test]
fn placeholder_handles_missing_parent_and_mismatched_full_parent_invalidate_provenance() {
    let original = provenance();
    for sql_handle in [false, true] {
        let mut changed = original.clone();
        if sql_handle {
            changed.statement.identity.sql_handle = "0x0000".into();
        } else {
            changed.statement.identity.plan_handle = "0x0000".into();
        }
        assert!(!changed.is_valid());
    }
    let mut changed = original.clone();
    changed.query = None;
    assert!(!changed.is_valid());
    changed = original.clone();
    changed.native_plan.as_mut().unwrap().query.qualifiers.insert("context_settings_id".into(), "6".into());
    assert!(!changed.is_valid(), "matching query numbers are not matching native parent identities");
    changed = original.clone();
    changed.native_plan.as_mut().unwrap().query.query_id = "8".into();
    assert!(!changed.is_valid());
    changed = original.clone();
    changed.read.partition = SourcePartition::Target;
    assert!(!changed.is_valid(), "a database namespace is not connected-cache ownership");
    changed = original;
    let SourcePartition::InstanceLocal { member: Some(member), .. } = changed.read.partition.clone() else { panic!() };
    changed.read.target = CollectionTarget::Shared(member);
    assert!(!changed.is_valid());
}

#[test]
fn current_read_bounds_do_not_freshen_original_cache_or_query_compilation_clocks() {
    let original = provenance();
    let bounds = ObservationBounds::new(&original.read.observed_from, &original.read.observed_until).unwrap();
    assert!(!bounds.contains(&original.query.as_ref().unwrap().qualifiers["initial_compile_at"]));
    assert!(!bounds.contains(&original.native_plan.as_ref().unwrap().qualifiers["initial_compile_at"]));
    assert!(original.is_valid(), "original native lifetimes are independent of the later read bounds");
    let mut later_role = original.clone();
    later_role.read.database_writable = Some(false);
    assert!(later_role.is_valid());
    assert_eq!(later_role.read.partition, original.read.partition);
    assert_eq!(later_role.statement, original.statement);
    assert_eq!(later_role.query, original.query);
    assert_eq!(later_role.native_plan, original.native_plan);
}

#[test]
fn retained_response_requires_an_actual_plan_from_the_provenance_database_engine() {
    let original = actual_response();
    assert!(original.is_valid());
    let mut estimated = original.clone();
    estimated.plan.kind = crate::PlanKind::Estimated;
    assert!(!estimated.is_valid(), "a valid native DTO cannot turn an estimated plan into retained actual evidence");
    let mut wrong_engine = original.clone();
    wrong_engine.plan.engine = crate::Engine::Postgres;
    assert!(!wrong_engine.is_valid());
    let mut wrong_source = original;
    wrong_source.provenance.statement.identity.sql_handle = "0x0000".into();
    assert!(!wrong_source.is_valid());
}

fn actual_response() -> RetainedActualPlan {
    let snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut plan = snapshot.queries()[0].plan.as_ref().unwrap().clone();
    plan.kind = crate::PlanKind::Actual;
    RetainedActualPlan { plan, provenance: provenance() }
}

#[test]
fn a_lookup_requires_its_selected_native_query_and_plan_even_without_saved_expectations() {
    let response = actual_response();
    let mut independent = RetainedActualPlanRequest {
        query_id: "7".into(),
        plan_id: Some("9".into()),
        expected_target: None,
        expected_query: None,
        expected_plan: None,
    };
    assert!(response.matches_request(&independent));
    independent.query_id = "8".into();
    assert!(!response.matches_request(&independent));
    independent.query_id = "7".into();
    independent.plan_id = Some("10".into());
    assert!(!response.matches_request(&independent));
    independent.plan_id = None;
    let mut unknown_query = response.clone();
    unknown_query.provenance.query = None;
    unknown_query.provenance.native_plan = None;
    assert!(unknown_query.is_valid(), "unlinked positive evidence remains a valid retained record");
    assert!(!unknown_query.matches_request(&independent), "it cannot answer a specific query lookup");
    let mut no_plan = response;
    no_plan.provenance.native_plan = None;
    assert!(no_plan.matches_request(&independent));
    independent.plan_id = Some("9".into());
    assert!(!no_plan.matches_request(&independent));
}

#[test]
fn a_lookup_checks_full_saved_qualifiers_and_canonical_database_identity() {
    let response = actual_response();
    let expected = request();
    assert!(response.matches_request(&expected));
    let mut reused_query = response.clone();
    reused_query.provenance.query.as_mut().unwrap().qualifiers.insert("context_settings_id".into(), "6".into());
    reused_query.provenance.native_plan.as_mut().unwrap().query = reused_query.provenance.query.clone().unwrap();
    assert!(reused_query.is_valid());
    assert!(!reused_query.matches_request(&expected));
    let mut reused_plan = response.clone();
    reused_plan
        .provenance
        .native_plan
        .as_mut()
        .unwrap()
        .qualifiers
        .insert("plan_hash".into(), "8899AABBCCDDEEFF".into());
    assert!(reused_plan.is_valid());
    assert!(!reused_plan.matches_request(&expected));
    let mut different_database = response.clone();
    let CollectionTarget::Database(database) = &mut different_database.provenance.read.target else { panic!() };
    database.database = "other-database".into();
    assert!(!different_database.matches_request(&expected));
    let mut canonical_alias = response;
    let CollectionTarget::Database(database) = &mut canonical_alias.provenance.read.target else { panic!() };
    let crate::DatabaseLocation::Server { host, .. } = &mut database.location else { panic!() };
    *host = "OWNED.INVALID".into();
    assert!(!canonical_alias.matches_request(&expected), "an endpoint alias alone cannot establish a native identity");
    let CollectionTarget::Database(database) = &mut canonical_alias.provenance.read.target else { panic!() };
    database.incarnation = Some("native-database-generation".into());
    let mut native_expected = expected;
    let Some(CollectionTarget::Database(database)) = &mut native_expected.expected_target else { panic!() };
    database.incarnation = Some("native-database-generation".into());
    assert!(canonical_alias.matches_request(&native_expected));
}
