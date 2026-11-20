use super::*;
use crate::{resources::test_support::replace, value::Val};

fn initial() -> Rec {
    Rec::of(&[
        ("database_name", Val::Text("master".into())),
        ("configured_classifier", Val::Int(0)),
        ("effective_classifier", Val::Int(0)),
        ("is_enabled", Val::Bool(false)),
        ("is_reconfiguration_pending", Val::Bool(false)),
        ("collision", Val::Int(0)),
    ])
}

#[test]
fn cleanup_refuses_live_sessions_or_replaced_native_objects_even_when_names_match() {
    let mut profile =
        ResourceProfile::new("culprit_smoke_0123456789abcdef01234567_grant_output_drain", STANDARD_POOL_MEMORY_PERCENT)
            .unwrap();
    let native = effective(&profile);
    profile.proof = Some(profile.validate_effective(&native).unwrap());
    assert!(profile.validate_cleanup(&native).is_ok());
    for (name, value) in [
        ("group_sessions", Val::Int(1)),
        ("pool_id", Val::Int(4)),
        ("group_id", Val::Int(4)),
        ("is_reconfiguration_pending", Val::Bool(true)),
    ] {
        assert!(profile.validate_cleanup(&replace(&native, &[(name, value)])).is_err(), "{name}");
    }
    let replaced = replace(
        &native,
        &[
            ("classifier_id", Val::Int(4096)),
            ("configured_classifier", Val::Int(4096)),
            ("effective_classifier", Val::Int(4096)),
        ],
    );
    assert!(profile.validate_cleanup(&replaced).is_err());
    let drop = profile.drop_resources(profile.proof.as_ref().unwrap(), false);
    assert!(drop.contains("ISNULL(OBJECT_ID"), "missing native object cannot bypass the final guard");
    assert!(drop.contains("ALTER RESOURCE GOVERNOR DISABLE"));
}

fn effective(profile: &ResourceProfile) -> Rec {
    Rec::of(&[
        ("configured_classifier", Val::Int(2048)),
        ("effective_classifier", Val::Int(2048)),
        ("classifier_id", Val::Int(2048)),
        ("is_enabled", Val::Bool(true)),
        ("is_reconfiguration_pending", Val::Bool(false)),
        ("pool_id", Val::Int(3)),
        ("group_id", Val::Int(3)),
        ("pool_name", Val::Text(profile.pool.clone())),
        ("group_name", Val::Text(profile.group.clone())),
        ("min_memory_percent", Val::Int(0)),
        ("max_memory_percent", Val::Int(profile.max_memory_percent)),
        ("max_memory_kb", Val::Int(100000)),
        ("target_memory_kb", Val::Int(80000)),
        ("grant_percent", Val::Float(100.0)),
        ("group_max_requests", Val::Int(0)),
        ("foreign_profiles", Val::Int(0)),
        ("group_sessions", Val::Int(0)),
    ])
}

#[test]
fn stored_effective_pending_foreign_and_unknown_configuration_cannot_be_overwritten() {
    assert!(!initial_enabled(&initial()).unwrap());
    for (name, value) in [
        ("configured_classifier", Val::Int(99)),
        ("effective_classifier", Val::Int(99)),
        ("is_reconfiguration_pending", Val::Bool(true)),
        ("collision", Val::Int(1)),
        ("configured_classifier", Val::Null),
    ] {
        assert!(initial_enabled(&replace(&initial(), &[(name, value)])).is_err(), "{name}");
    }
}

#[test]
fn effective_native_policy_and_capacity_must_precede_every_registered_actor_route() {
    let mut profile =
        ResourceProfile::new("culprit_smoke_0123456789abcdef01234567_grant_output_drain", STANDARD_POOL_MEMORY_PERCENT)
            .unwrap();
    let native = effective(&profile);
    profile.proof = Some(profile.validate_effective(&native).unwrap());
    for index in 0..MAX_FIXTURE_REQUESTS {
        let route = profile.route(index).unwrap();
        let row = Rec::of(&[
            ("actor_application", Val::Text(route.application.clone())),
            ("pool_name", Val::Text(route.pool.clone())),
            ("group_name", Val::Text(route.group.clone())),
            ("pool_id", Val::Int(route.pool_id)),
            ("group_id", Val::Int(route.group_id)),
            ("configured_classifier", Val::Int(route.classifier_id)),
            ("effective_classifier", Val::Int(route.classifier_id)),
            ("is_reconfiguration_pending", Val::Bool(false)),
            ("is_enabled", Val::Bool(true)),
            ("min_memory_percent", Val::Int(0)),
            ("max_memory_percent", Val::Int(route.max_memory_percent)),
            ("max_memory_kb", Val::Int(100000)),
            ("target_memory_kb", Val::Int(80000)),
            ("grant_percent", Val::Float(100.0)),
            ("group_max_requests", Val::Int(0)),
        ]);
        assert!(route.matches(&row).unwrap());
        assert!(!route.matches(&replace(&row, &[("group_id", Val::Int(2))])).unwrap());
        assert!(!route.matches(&replace(&row, &[("actor_application", Val::Text("culprit".into()))])).unwrap());
    }
    assert!(profile.route(MAX_FIXTURE_REQUESTS).is_err());
    for (name, value) in [
        ("effective_classifier", Val::Int(9)),
        ("group_max_requests", Val::Int(1)),
        ("grant_percent", Val::Float(25.0)),
        ("max_memory_percent", Val::Int(100)),
        ("foreign_profiles", Val::Int(1)),
        ("max_memory_kb", Val::Int(0)),
        ("target_memory_kb", Val::Null),
        ("max_memory_kb", Val::Int((MAX_FIXTURE_MEMORY_KIB / 3 + 1) as i64)),
    ] {
        assert!(profile.validate_effective(&replace(&native, &[(name, value)])).is_err(), "{name}");
    }
}

#[test]
fn larger_scalar_queue_pool_keeps_exact_native_policy_and_the_two_gib_capacity_ceiling() {
    use super::super::Case;
    assert_eq!(Case::OriginalScalarGrant.pool_memory_percent(), SCALAR_QUEUE_POOL_MEMORY_PERCENT);
    for case in
        [Case::Reservations, Case::DrainOutput, Case::OriginalScalarCacheControl, Case::OriginalScalarSourcePhases]
    {
        assert_eq!(case.pool_memory_percent(), STANDARD_POOL_MEMORY_PERCENT);
    }
    let mut profile = ResourceProfile::new(
        "culprit_smoke_0123456789abcdef01234567_grant_original_scalar",
        Case::OriginalScalarGrant.pool_memory_percent(),
    )
    .unwrap();
    let native = effective(&profile);
    profile.proof = Some(profile.validate_effective(&native).unwrap());
    let route = profile.route(MAX_FIXTURE_REQUESTS - 1).unwrap();
    let mut actor_columns = native.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    actor_columns.push(("actor_application", Val::Text(route.application.clone())));
    let actor = Rec::of(&actor_columns);
    assert!(route.matches(&actor).unwrap());
    assert_eq!(profile.snapshot()["pool_memory_percent"], SCALAR_QUEUE_POOL_MEMORY_PERCENT);
    for observed_percent in [STANDARD_POOL_MEMORY_PERCENT, 0, 100] {
        let changed = replace(&native, &[("max_memory_percent", Val::Int(observed_percent))]);
        assert!(profile.validate_effective(&changed).is_err());
        assert!(route.matches(&replace(&actor, &[("max_memory_percent", Val::Int(observed_percent))])).is_err());
    }
    let maximum_pool = MAX_FIXTURE_MEMORY_KIB / MAX_FIXTURE_REQUESTS as u64;
    let at_limit = replace(
        &native,
        &[("max_memory_kb", Val::Int(maximum_pool as i64)), ("target_memory_kb", Val::Int(maximum_pool as i64))],
    );
    assert!(profile.validate_effective(&at_limit).is_ok());
    let above_limit = replace(&at_limit, &[("max_memory_kb", Val::Int(maximum_pool as i64 + 1))]);
    assert!(profile.validate_effective(&above_limit).is_err());
    for invalid_percent in [0, 101] {
        assert!(ResourceProfile::new("culprit_smoke_0123456789abcdef01234567", invalid_percent).is_err());
    }
}
