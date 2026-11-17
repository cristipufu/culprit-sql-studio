use super::*;
use crate::{Engine, SourceState};

pub(in crate::telemetry) fn read_context() -> NativeReadContext {
    NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "owned-db", "owned.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("owned-machine"),
                native: NativeIdentity::new("owned-instance"),
            }),
            epoch: Some("owned-process-epoch".into()),
        },
        observed_from: "2020-01-01T12:00:00.000000000Z".into(),
        observed_until: "2020-01-01T12:00:01.000000000Z".into(),
        database_writable: Some(true),
    }
}

#[test]
fn request_plan_cache_ownership_ignores_poll_clocks_but_never_native_lifetime_or_scope() {
    let context = read_context();
    let actor = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "login-generation".into() },
        request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-start".into() }),
        transaction: None,
    };
    let statement = NativeIdentity::new("native-statement");
    let original = context.request_key(&actor, &statement).unwrap();
    let mut later = context.clone();
    later.observed_from = "2020-01-01T12:00:02Z".into();
    later.observed_until = "2020-01-01T12:00:03Z".into();
    assert_eq!(later.request_key(&actor, &statement).as_deref(), Some(original.as_str()));
    for change in ["member", "epoch", "role", "session", "request", "statement"] {
        let mut changed = context.clone();
        let mut current = actor.clone();
        let mut sql = statement.clone();
        match change {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    panic!()
                };
                member.native.id = "other-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = Some("new-process".into());
            }
            "role" => changed.database_writable = Some(false),
            "session" => current.session.generation = "new-login".into(),
            "request" => current.request.as_mut().unwrap().generation = "new-request".into(),
            _ => sql.id = "other-statement".into(),
        }
        assert_ne!(changed.request_key(&current, &sql).as_deref(), Some(original.as_str()), "{change}");
    }
    later.partition = SourcePartition::Target;
    assert_eq!(later.request_key(&actor, &statement), None, "a logical route cannot own a cached native plan");
    let mut idle = actor;
    idle.request = None;
    assert_eq!(context.request_key(&idle, &statement), None);
}

#[test]
fn known_local_owner_and_canonical_bounds_round_trip_without_changing_native_facts() {
    let original = read_context();
    assert!(original.is_valid());
    assert!(original.has_qualified_local_owner());
    let recovered: NativeReadContext = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(recovered, original);
    let mut equal_instant = original.clone();
    equal_instant.observed_until = "2020-01-01T14:00:00+02:00".into();
    assert!(equal_instant.is_valid(), "inclusive bounds compare instants, not timestamp spellings");
    equal_instant.observed_until = "2020-01-01T13:00:00+02:00".into();
    assert!(!equal_instant.is_valid(), "a later-looking local clock can still precede the UTC start");
}

#[test]
fn missing_member_or_epoch_remains_valid_raw_context_without_qualified_local_ownership() {
    let original = read_context();
    let SourcePartition::InstanceLocal { member, epoch } = &original.partition else { panic!() };
    for partition in [
        SourcePartition::InstanceLocal { member: None, epoch: None },
        SourcePartition::InstanceLocal { member: member.clone(), epoch: None },
        SourcePartition::InstanceLocal { member: None, epoch: epoch.clone() },
        SourcePartition::Target,
    ] {
        let context = NativeReadContext { partition, database_writable: None, ..original.clone() };
        assert!(context.is_valid());
        assert!(!context.has_qualified_local_owner());
    }
}

#[test]
fn invalid_targets_member_namespace_and_observation_clocks_cannot_qualify_a_read() {
    let original = read_context();
    let mut invalid_target = original.clone();
    let CollectionTarget::Database(target) = &mut invalid_target.target else { panic!() };
    target.database.clear();
    assert!(!invalid_target.is_valid());
    assert!(!invalid_target.has_qualified_local_owner());
    let mut wrong_engine = original.clone();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut wrong_engine.partition else { panic!() };
    member.engine = Engine::Postgres;
    assert!(!wrong_engine.is_valid());
    for until in
        ["2020-01-01T11:59:59Z", "2020-01-01T12:00:01", "2020-02-30T12:00:01Z", "2020-01-01T12:00:01.0000000001Z"]
    {
        let context = NativeReadContext { observed_until: until.into(), ..original.clone() };
        assert!(!context.is_valid(), "{until}");
        assert!(!context.has_qualified_local_owner(), "{until}");
    }
}

fn snapshot_source(context: &NativeReadContext) -> CollectionDescription {
    CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: "owned-native-statement-cache".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::StatementGrants,
            records: 1,
            observed_from: context.observed_from.clone(),
            observed_until: context.observed_until.clone(),
            sources: vec![StateSourceCoverage::collected("owned-grants".into(), 1)],
        }),
    }
}

#[test]
fn snapshot_conversion_preserves_native_partition_and_partial_unknowns_without_inventing_role() {
    let original = read_context();
    let source = snapshot_source(&original);
    let read = NativeReadContext::from_snapshot(&source).unwrap();
    assert_eq!(read.target, original.target);
    assert_eq!(read.partition, original.partition);
    assert_eq!(read.database_writable, None, "the snapshot did not record updateability");
    let mut unknown = source.clone();
    unknown.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let SourceObservation::Snapshot(snapshot) = &mut unknown.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed("owned-grants".into(), 1, false);
    assert_eq!(snapshot.sources[0].state, SourceState::Partial);
    let read = NativeReadContext::from_snapshot(&unknown).unwrap();
    assert!(!read.has_qualified_local_owner());
    let SourceObservation::Snapshot(snapshot) = &mut unknown.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::collected("owned-grants".into(), 1);
    assert!(NativeReadContext::from_snapshot(&unknown).is_err(), "unknown owners cannot acquire complete scope");
    let mut backwards = source;
    let SourceObservation::Snapshot(snapshot) = &mut backwards.observation else { panic!() };
    snapshot.observed_until = "2020-01-01T11:59:59Z".into();
    assert!(NativeReadContext::from_snapshot(&backwards).is_err());
}

#[test]
fn a_current_member_and_role_cannot_supply_query_historys_native_population_tuple() {
    let mut context = read_context();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { panic!() };
    member.native.id = "7".into();
    let member_qualifiers = member.native.qualifiers.clone();
    let binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: vec![QueryHistoryPopulationBindingEntry {
            population: NativeIdentity::new("7"),
            qualifiers: [("native-execution-population".into(), "retained-group-7".into())].into(),
        }],
    };
    for role in [Some(true), Some(false), None] {
        context.database_writable = role;
        assert!(context.has_qualified_local_owner());
        assert!(binding.resolve(&member_qualifiers).is_err(), "read ownership is not the execution's native tuple");
    }
}

#[test]
fn activity_header_and_frame_share_one_native_context_without_changing_unknown_ownership() {
    let source = super::super::activity_frame::tests::source(0);
    let expected = NativeReadContext::from_activity(&source).unwrap();
    let frame = ActivityFrameEvidence::new(&source, &[], true).unwrap();
    assert_eq!(expected, frame.context());
    assert_eq!(expected.partition, SourcePartition::Target);
    assert!(!expected.has_qualified_local_owner());
    let mut changed = source.clone();
    let SourceObservation::Activity(header) = &mut changed.observation else { panic!() };
    header.coverage.source = "different-native-source".into();
    assert!(NativeReadContext::from_activity(&changed).is_err());
    assert!(ActivityFrameEvidence::new(&changed, &[], true).is_err());
}

#[test]
fn an_earlier_scope_requires_exact_known_owner_role_target_and_ordered_native_bounds() {
    let original = read_context();
    let current = NativeReadContext {
        observed_from: "2020-01-01T12:00:02Z".into(),
        observed_until: "2020-01-01T12:00:03Z".into(),
        ..original.clone()
    };
    assert!(current.matches_earlier_scope(&original));
    for field in ["target", "member", "epoch", "role", "unknown-role", "unknown-owner", "clock"] {
        let mut changed = current.clone();
        match field {
            "target" => {
                let CollectionTarget::Database(database) = &mut changed.target else { panic!() };
                database.database = "other".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    panic!()
                };
                member.native.id = "another-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = Some("new-native-epoch".into());
            }
            "role" => changed.database_writable = Some(false),
            "unknown-role" => changed.database_writable = None,
            "unknown-owner" => changed.partition = SourcePartition::Target,
            _ => changed.observed_from = "2020-01-01T12:00:00Z".into(),
        }
        assert!(!changed.matches_earlier_scope(&original), "{field}");
    }
    let unknown = NativeReadContext { database_writable: None, ..original };
    assert!(!current.matches_earlier_scope(&unknown));
}
