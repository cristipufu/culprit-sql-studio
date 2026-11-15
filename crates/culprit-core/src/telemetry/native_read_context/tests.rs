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
