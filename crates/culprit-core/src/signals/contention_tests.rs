use super::*;
use crate::{
    Engine, ResetMarker,
    telemetry::{NativeIdentity, SharedResourceIdentity, SharedResourceKind},
};

fn owner(member: &str) -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Node,
        namespace: NativeIdentity::new("native-cluster-private"),
        native: NativeIdentity::new(member),
    }
}

fn scan(taken_at: &str, count: f64) -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let mut index = snapshot.indexes[0].clone();
    index.schema = "shop".into();
    index.table = "orders".into();
    index.name = "ix_orders".into();
    index.contention = Some(IndexContention {
        source_owner: Some(owner("native-member-a-private")),
        reset: ResetMarker::Generation("native-index-generation-1".into()),
        row_lock_waits: count,
        row_lock_wait_ms: count * 2.0,
        lock_escalations: count,
        ..Default::default()
    });
    snapshot.indexes = vec![index];
    snapshot.taken_at = taken_at.into();
    snapshot
}

fn before() -> Snapshot {
    scan("2026-11-14T10:00:00Z", 0.0)
}

fn after() -> Snapshot {
    scan("2026-11-14T11:00:00Z", 100.0)
}

fn counters(snapshot: &mut Snapshot) -> &mut IndexContention {
    snapshot.indexes[0].contention.as_mut().unwrap()
}

fn assert_uncomparable(previous: &Snapshot, current: &Snapshot) {
    assert!(ContentionBaseline::from_scan(previous, current).is_none());
    assert!(EscalationBaseline::from_scan(previous, current).is_none());
}

#[test]
fn a_member_switch_with_unknown_counter_ownership_cannot_create_recent_waits_or_escalations() {
    let mut previous = before();
    let mut current = after();
    // A separate pooled probe can report the same database and start time while these points came from A and B.
    current.server.started_at = previous.server.started_at.clone();
    for snapshot in [&mut previous, &mut current] {
        counters(snapshot).source_owner = None;
        counters(snapshot).reset = ResetMarker::Unknown;
    }
    assert!(previous.same_database(&current));
    assert_uncomparable(&previous, &current);
    assert_eq!(counters(&mut current).lock_escalations, 100.0, "the original positive point remains available");
    assert_eq!(counters(&mut current).lock_wait_ms(), 200.0);
}

#[test]
fn a_known_reset_without_a_native_member_and_a_native_member_without_a_reset_are_both_unproven() {
    for reset in [ResetMarker::At("2026-11-14T09:00:00Z".into()), ResetMarker::Generation("native-1".into())] {
        let (mut previous, mut current) = (before(), after());
        for snapshot in [&mut previous, &mut current] {
            counters(snapshot).source_owner = None;
            counters(snapshot).reset = reset.clone();
        }
        assert_uncomparable(&previous, &current);
    }
    let (mut previous, mut current) = (before(), after());
    counters(&mut previous).reset = ResetMarker::Unknown;
    counters(&mut current).reset = ResetMarker::Unknown;
    assert_uncomparable(&previous, &current);
}

#[test]
fn matching_native_member_and_counter_generation_establish_deltas_independently_of_probe_uptime() {
    for reset in [ResetMarker::At("2026-11-14T09:00:00Z".into()), ResetMarker::Generation("native-1".into())] {
        let (mut previous, mut current) = (before(), after());
        for snapshot in [&mut previous, &mut current] {
            counters(snapshot).reset = reset.clone();
            snapshot.server.started_at = None;
        }
        let contention = ContentionBaseline::from_scan(&previous, &current).unwrap();
        let delta = contention.since(&current.indexes[0]).unwrap();
        assert_eq!((delta.waits, delta.wait_ms), (100.0, 200.0));
        let escalations = EscalationBaseline::from_scan(&previous, &current).unwrap();
        assert_eq!(escalations.since(&current, "shop", "orders"), Some(100.0));
    }
}

#[test]
fn an_equal_native_reset_cannot_be_after_the_earlier_counter_point() {
    let (mut previous, mut current) = (before(), after());
    for reset in ["2026-11-14T10:30:00Z", "2026-11-14T12:00:00Z"] {
        counters(&mut previous).reset = ResetMarker::At(reset.into());
        counters(&mut current).reset = ResetMarker::At(reset.into());
        assert_uncomparable(&previous, &current);
    }
    counters(&mut previous).reset = ResetMarker::At("2026-11-14T09:00:00Z".into());
    counters(&mut current).reset = counters(&mut previous).reset.clone();
    let mut locks = ContentionBaseline::from_scan(&previous, &current).unwrap();
    let mut escalations = EscalationBaseline::from_scan(&previous, &current).unwrap();
    let future_reset = ResetMarker::At("2026-11-14T10:30:00Z".into());
    counters(&mut current).reset = future_reset.clone();
    locks.indexes.values_mut().for_each(|point| point.reset = future_reset.clone());
    escalations
        .tables
        .values_mut()
        .flat_map(|points| points.values_mut())
        .for_each(|point| point.reset = future_reset.clone());
    assert!(locks.since(&current.indexes[0]).is_none());
    assert_eq!(escalations.since(&current, "shop", "orders"), None);
    assert_eq!(counters(&mut current).lock_escalations, 100.0);
}

#[test]
fn a_changed_member_or_counter_generation_never_inherits_the_old_members_zero() {
    let previous = before();
    for changed in ["member", "generation", "owner-incarnation", "invalid-reset", "invalid-owner", "pool"] {
        let mut current = after();
        let point = counters(&mut current);
        match changed {
            "member" => point.source_owner = Some(owner("native-member-b")),
            "generation" => point.reset = ResetMarker::Generation("native-index-generation-2".into()),
            "owner-incarnation" => {
                point.source_owner.as_mut().unwrap().native =
                    NativeIdentity::new("native-member-a-private").qualified("epoch", "new");
            }
            "invalid-reset" => point.reset = ResetMarker::Generation(" ".into()),
            "invalid-owner" => point.source_owner.as_mut().unwrap().namespace.id.clear(),
            "pool" => point.source_owner.as_mut().unwrap().kind = SharedResourceKind::Pool,
            _ => unreachable!(),
        }
        assert_uncomparable(&previous, &current);
    }
}

#[test]
fn table_sums_cannot_hide_changed_index_membership_mixed_owners_or_an_individual_counter_reset() {
    let mut previous = scan("2026-11-14T10:00:00Z", 100.0);
    let mut second = previous.indexes[0].clone();
    second.name = "ix_other".into();
    second.contention.as_mut().unwrap().reset = ResetMarker::Generation("native-index-generation-2".into());
    previous.indexes.push(second);
    let mut current = previous.clone();
    current.taken_at = "2026-11-14T11:00:00Z".into();
    let baseline = EscalationBaseline::from_scan(&previous, &current).unwrap();
    assert_eq!(baseline.since(&current, "shop", "orders"), Some(0.0));
    for changed in ["removed", "added", "renamed", "missing", "mixed", "reset", "offsetting-shrink", "duplicate"] {
        let mut changed_snapshot = current.clone();
        match changed {
            "removed" => {
                changed_snapshot.indexes.pop();
            }
            "added" => {
                let mut index = changed_snapshot.indexes[0].clone();
                index.name = "ix_new".into();
                changed_snapshot.indexes.push(index);
            }
            "renamed" => changed_snapshot.indexes[1].name = "ix_replacement".into(),
            "missing" => changed_snapshot.indexes[1].contention = None,
            "mixed" => changed_snapshot.indexes[1].contention.as_mut().unwrap().source_owner = Some(owner("member-b")),
            "reset" => {
                changed_snapshot.indexes[1].contention.as_mut().unwrap().reset =
                    ResetMarker::Generation("new-index".into())
            }
            "offsetting-shrink" => {
                changed_snapshot.indexes[0].contention.as_mut().unwrap().lock_escalations = 0.0;
                changed_snapshot.indexes[1].contention.as_mut().unwrap().lock_escalations = 200.0;
            }
            "duplicate" => changed_snapshot.indexes[1].name = changed_snapshot.indexes[0].name.clone(),
            _ => unreachable!(),
        }
        assert_eq!(baseline.since(&changed_snapshot, "shop", "orders"), None, "{changed}");
        assert!(EscalationBaseline::from_scan(&previous, &changed_snapshot).is_none(), "{changed}");
    }
}

#[test]
fn retained_baselines_revalidate_current_proof_and_never_accept_invalid_or_shrinking_counts() {
    let previous = scan("2026-11-14T10:00:00Z", 50.0);
    let current = after();
    let locks = ContentionBaseline::from_scan(&previous, &current).unwrap();
    let escalations = EscalationBaseline::from_scan(&previous, &current).unwrap();
    for changed in ["owner", "unknown-reset", "shrink", "nan", "negative-component"] {
        let mut changed_snapshot = current.clone();
        let point = counters(&mut changed_snapshot);
        match changed {
            "owner" => point.source_owner = None,
            "unknown-reset" => point.reset = ResetMarker::Unknown,
            "shrink" => {
                point.row_lock_waits = 1.0;
                point.lock_escalations = 1.0;
            }
            "nan" => {
                point.row_lock_waits = f64::NAN;
                point.lock_escalations = f64::NAN;
            }
            "negative-component" => {
                point.page_lock_wait_ms = -1.0;
                point.lock_escalations = -1.0;
            }
            _ => unreachable!(),
        }
        assert!(locks.since(&changed_snapshot.indexes[0]).is_none(), "{changed}");
        assert_eq!(escalations.since(&changed_snapshot, "shop", "orders"), None, "{changed}");
    }
}

#[test]
fn shared_snapshots_preserve_native_counter_relationships_without_publishing_native_member_identifiers() {
    let previous = before();
    let mut current = after();
    current.contention_before = ContentionBaseline::from_scan(&previous, &current);
    current.escalations_before = EscalationBaseline::from_scan(&previous, &current);
    let shared = crate::redact::redact_snapshot(&current);
    let serialized = serde_json::to_string(&shared).unwrap();
    assert!(!serialized.contains("native-member-a-private"));
    assert!(!serialized.contains("native-cluster-private"));
    assert_eq!(shared.contention_before.as_ref().unwrap().since(&shared.indexes[0]).unwrap().waits, 100.0);
    assert_eq!(shared.escalations_before.as_ref().unwrap().since(&shared, "shop", "orders"), Some(100.0));
    assert_eq!(counters(&mut current).source_owner, Some(owner("native-member-a-private")));
}
