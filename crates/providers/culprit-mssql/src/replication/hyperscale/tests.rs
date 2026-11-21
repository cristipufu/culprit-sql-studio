use super::*;
use crate::{
    activity::database_context::tests::row,
    instance::test_support::metadata as platform,
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
const LOCAL: &str = "91487244-730d-4b83-8e49-d482241c7288";
const REMOTE: &str = "11111111-1111-1111-1111-111111111111";
const AT: &str = "2020-01-01T06:00:01Z";
pub(crate) fn metadata(edition: &str, at: &str) -> Rec {
    Rec::of(&[
        ("selected_database_id", Val::Int(7)),
        ("observer_replica_id", Val::Text(LOCAL.into())),
        ("database_edition", Val::Text(edition.into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text(at.into())),
        ("supported_columns", Val::Int(8)),
    ])
}
pub(crate) fn topology(role: u8) -> Rec {
    Rec::of(&[
        ("selected_database_id", Val::Int(7)),
        ("observer_replica_id", Val::Text(LOCAL.into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ("topology_present", Val::Int(1)),
        ("replica_id", Val::Text(REMOTE.into())),
        ("replica_server_name", Val::Text("logical-server.fixture.invalid".into())),
        ("replica_database_name", Val::Text("app".into())),
        ("is_local", Val::Int(0)),
        ("replica_role", Val::Int(i64::from(role))),
        ("native_role", Val::Text("NATIVE ROLE".into())),
        ("local_database_id", Val::Int(70)),
        ("parent_replica_id", if role == 1 { Val::Text(LOCAL.into()) } else { Val::Null }),
    ])
}
async fn owner() -> Context {
    let mut io = FakeRead::new(vec![Ok(vec![replace(&platform(), &[("edition", Val::Int(5))])]), Ok(vec![row(5)])]);
    let result = Context::read(&mut io, "fixture.invalid", 1433).await.unwrap();
    assert_eq!(io.queries(), 2);
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
    result
}
fn replies(rows: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    vec![Ok(vec![metadata("Hyperscale", AT)]), Ok(rows)]
}
#[tokio::test]
async fn native_hyperscale_roles_keep_parent_edges_nulls_and_remote_local_database_ids_as_facts() {
    let owner = owner().await;
    for role in [0, 1, 2, 3, 255] {
        let mut reader = FakeRead::new(replies(vec![topology(role)]));
        let (part, witness) = read(&mut reader, &owner, 64).await.unwrap();
        let Part::Observed { facts, complete, limited, .. } = part else { panic!() };
        assert!(!complete && !limited);
        assert_eq!(facts.len(), 1);
        let ReplicationFact::HyperscaleReplica(point) = &facts[0] else { panic!() };
        assert_eq!(point.replica_role, Some(role));
        assert_eq!(point.local_database_id, Some(70));
        assert_eq!(point.parent_replica_id.as_deref(), (role == 1).then_some(LOCAL));
        assert_eq!(witness.unwrap().scope.database_id, 7);
        assert_eq!(reader.queries(), 2);
        assert!(reader.calls[1].sql.contains("sys.dm_hs_database_replicas(@selected_database_id)"));
        assert!(reader.calls[1].sql.contains("@selected_database_id int=DB_ID()"));
        assert!(!reader.calls[1].sql.contains("SELECT TOP(65) *"));
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let row = replace(
        &topology(0),
        &[("replica_id", Val::Text(LOCAL.into())), ("is_local", Val::Int(1)), ("local_database_id", Val::Int(7))],
    );
    let mut reader = FakeRead::new(replies(vec![row]));
    assert!(matches!(read(&mut reader, &owner, 64).await.unwrap().0, Part::Observed { .. }));
}
#[tokio::test]
async fn native_nulls_are_unknown_and_one_qualified_placeholder_is_partial_not_healthy_absence() {
    let owner = owner().await;
    let fields = [
        "replica_server_name",
        "replica_database_name",
        "is_local",
        "replica_role",
        "native_role",
        "local_database_id",
        "parent_replica_id",
    ];
    let nulls = replace(&topology(0), &fields.map(|key| (key, Val::Null)));
    let mut reader = FakeRead::new(replies(vec![nulls]));
    let (part, _) = read(&mut reader, &owner, 64).await.unwrap();
    let Part::Observed { facts, complete, .. } = part else { panic!() };
    assert!(!complete);
    let ReplicationFact::HyperscaleReplica(point) = &facts[0] else { panic!() };
    assert_eq!(point.replica_role, None);
    assert_eq!(point.is_local, None);
    assert_eq!(point.local_database_id, None);
    let mut changes = fields.map(|key| (key, Val::Null)).to_vec();
    changes.extend([("topology_present", Val::Int(0)), ("replica_id", Val::Null)]);
    let empty = replace(&topology(0), &changes);
    let mut reader = FakeRead::new(replies(vec![empty.clone()]));
    let (part, witness) = read(&mut reader, &owner, 64).await.unwrap();
    assert!(witness.is_some());
    let selected = super::super::selection::SelectedRead { parts: vec![part] };
    let (rows, coverage) = selected.into_records(true, None);
    assert!(rows.is_empty());
    assert_eq!(coverage[0].state, culprit_core::SourceState::Partial);
    assert!(!coverage[0].complete_scope);
    let mut reader = FakeRead::new(replies(vec![replace(&empty, &[("local_database_id", Val::Int(7))])]));
    assert!(read(&mut reader, &owner, 64).await.is_err(), "a contradictory empty placeholder is malformed");
}
#[tokio::test]
async fn native_layout_tier_and_permission_gaps_remain_independent_and_never_fall_back_to_other_queries() {
    let owner = owner().await;
    for case in ["tier", "layout", "permission", "interface"] {
        let data = match case {
            "tier" => vec![Ok(vec![metadata("General Purpose", AT)])],
            "layout" => vec![Ok(vec![replace(&metadata("Hyperscale", AT), &[("supported_columns", Val::Int(7))])])],
            _ => vec![
                Ok(vec![metadata("Hyperscale", AT)]),
                Ok(vec![crate::replication::tests::unavailable(if case == "permission" { 229 } else { 208 })]),
            ],
        };
        let queries = data.len();
        let mut reader = FakeRead::new(data);
        let (part, witness) = read(&mut reader, &owner, 64).await.unwrap();
        assert!(witness.is_none());
        if case == "tier" {
            assert!(matches!(part, Part::NotApplicable { .. }));
        } else {
            assert!(matches!(part, Part::Unavailable { .. }));
        }
        assert_eq!(reader.queries() as usize, queries);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn native_scope_role_clock_and_identity_contradictions_stop_without_partial_publication() {
    let owner = owner().await;
    for change in [
        "selected_database_id",
        "observer_replica_id",
        "updateability",
        "observed_at",
        "replica_id",
        "local_database_id",
        "parent_replica_id",
    ] {
        let (row, value) = match change {
            "selected_database_id" => (topology(1), Val::Int(8)),
            "observer_replica_id" => (topology(1), Val::Text(REMOTE.into())),
            "updateability" => (topology(1), Val::Text("READ_ONLY".into())),
            "observed_at" => (topology(1), Val::Text("2019-01-01T00:00:00Z".into())),
            "replica_id" => (topology(1), Val::Text("logical-server".into())),
            "local_database_id" => (
                replace(&topology(0), &[("replica_id", Val::Text(LOCAL.into())), ("is_local", Val::Int(1))]),
                Val::Int(8),
            ),
            _ => (topology(1), Val::Text(REMOTE.into())),
        };
        let mut reader = FakeRead::new(replies(vec![replace(&row, &[(change, value)])]));
        assert!(read(&mut reader, &owner, 64).await.is_err(), "{change}");
        assert_eq!(reader.queries(), 2);
    }
    let mut reader = FakeRead::new(replies(vec![topology(1), topology(1)]));
    assert!(read(&mut reader, &owner, 64).await.is_err(), "duplicate native replica IDs");
}
#[tokio::test]
async fn bounded_topology_uses_one_lookahead_and_keeps_a_row_limit_without_complete_tree_claim() {
    let owner = owner().await;
    let rows = (0..65)
        .map(|id| replace(&topology(0), &[("replica_id", Val::Text(format!("11111111-1111-1111-1111-{id:012}")))]))
        .collect();
    let mut reader = FakeRead::new(replies(rows));
    let (part, _) = read(&mut reader, &owner, 1000).await.unwrap();
    let Part::Observed { facts, limited, complete, .. } = part else { panic!() };
    assert_eq!(facts.len(), 64);
    assert!(limited);
    assert!(!complete);
    assert_eq!(reader.calls[1].limit, 65);
    let (_, coverage) = super::super::selection::SelectedRead {
        parts: vec![Part::Observed { selection: Selection::Hyperscale, facts, limited, complete }],
    }
    .into_records(true, None);
    assert_eq!(coverage[0].records, 64);
    assert_eq!(coverage[0].stopped, Some(CollectionStop::RowLimit));
    let mut reader = FakeRead::new(vec![]);
    let (part, _) = read(&mut reader, &owner, 0).await.unwrap();
    assert!(matches!(part, Part::Unavailable { .. }));
    assert_eq!(reader.queries(), 0);
}
#[tokio::test]
async fn terminal_native_stops_and_protocol_errors_cannot_reopen_the_topology_owner() {
    let owner = owner().await;
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::SourceChanged,
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![metadata("Hyperscale", AT)]), Err(ReadError::Stopped(stop))]);
        let error = read(&mut reader, &owner, 64).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata("Hyperscale", AT)]),
        Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "retired native owner"))),
    ]);
    assert!(read(&mut reader, &owner, 64).await.is_err(), "only completed native availability rows permit later work");
}
