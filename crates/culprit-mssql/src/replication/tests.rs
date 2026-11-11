use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

const ID: &str = "11111111-2222-3333-4444-555555555555";
fn layout(enabled: Val, lag: i64) -> Rec {
    Rec::of(&[("hadr_enabled", enabled), ("lag_column", Val::Int(lag))])
}
fn availability() -> Rec {
    Rec::of(&[
        ("group_id", Val::Text(ID.into())),
        ("group_database_id", Val::Text(ID.into())),
        ("replica_id", Val::Text(ID.into())),
        ("native_name", Val::Text("secondary".into())),
        ("is_local", Val::Int(0)),
        ("is_primary", Val::Int(0)),
        ("state", Val::Int(0)),
        ("native_state", Val::Text("NOT SYNCHRONIZING".into())),
        ("mode", Val::Int(0)),
        ("native_mode", Val::Text("ASYNCHRONOUS_COMMIT".into())),
        ("is_suspended", Val::Int(1)),
        ("suspend_reason", Val::Int(2)),
        ("synchronization_health", Val::Int(0)),
        ("log_send_queue_kb", Val::Int(9_007_199_254_740_993)),
        ("redo_queue_kb", Val::Int(42)),
        ("log_send_rate_kb_per_second", Val::Null),
        ("redo_rate_kb_per_second", Val::Int(0)),
        ("secondary_lag_seconds", Val::Int(31)),
    ])
}
pub(crate) fn geo() -> Rec {
    Rec::of(&[
        ("link_id", Val::Text(ID.into())),
        ("partner_server", Val::Text("replica.example".into())),
        ("partner_database", Val::Text("shop".into())),
        ("role", Val::Int(0)),
        ("state", Val::Int(2)),
        ("native_state", Val::Text("CATCH_UP".into())),
        ("lag_seconds", Val::Int(3)),
        ("last_replication_at", Val::Text("2020-01-01T01:00:00+01:00".into())),
    ])
}
#[tokio::test]
async fn native_ag_identity_direction_units_and_optional_layout_are_shared_with_the_app() {
    for lag in [0, 1] {
        let row =
            if lag == 0 { replace(&availability(), &[("secondary_lag_seconds", Val::Null)]) } else { availability() };
        let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), lag)]), Ok(vec![row])]);
        let facts = read(&mut reader, Selection::AvailabilityGroup, 100).await.unwrap();
        assert!(reader.calls[1].sql.contains("WHERE drs.database_id=DB_ID()"));
        assert!(reader.calls[1].sql.contains("LEFT JOIN sys.availability_replicas"));
        assert_eq!(reader.calls[1].sql.contains("drs.secondary_lag_seconds"), lag == 1);
        let ReplicationFact::AvailabilityGroup(raw) = &facts[0] else { panic!("AG") };
        assert_eq!(raw.log_send_queue_kb.as_deref(), Some("9007199254740993"));
        assert_eq!(raw.log_send_rate_kb_per_second, None);
        assert_eq!(raw.redo_rate_kb_per_second.as_deref(), Some("0"));
        let app = application(&facts[0]).unwrap();
        assert_eq!(app.role, ReplicaRole::Replica);
        assert_eq!(app.condition, ReplicaCondition::Stopped);
        assert_eq!(app.sync.as_deref(), Some("asynchronous commit"));
        assert_eq!(app.lag_bytes, Some(42.0 * 1024.0));
        assert_eq!(app.lag_seconds, if lag == 1 { Some(31.0) } else { None });
    }
}
#[tokio::test]
async fn unknown_roles_and_missing_metadata_are_retained_without_inventing_replica_direction() {
    let unknown = replace(
        &availability(),
        &[
            ("is_local", Val::Null),
            ("is_primary", Val::Null),
            ("native_name", Val::Null),
            ("mode", Val::Null),
            ("native_mode", Val::Null),
        ],
    );
    let local_primary = replace(
        &availability(),
        &[
            ("replica_id", Val::Text("22222222-2222-3333-4444-555555555555".into())),
            ("is_local", Val::Int(1)),
            ("is_primary", Val::Int(1)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![unknown, local_primary])]);
    let rows = read(&mut reader, Selection::AvailabilityGroup, 100).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(!direction_known(&rows[0]));
    assert!(direction_known(&rows[1]));
    assert!(rows.iter().all(|row| application(row).is_none()));
}
#[tokio::test]
async fn disabled_ag_is_known_empty_but_failed_or_unknown_probes_are_never_empty_successes() {
    let mut disabled = FakeRead::new(vec![Ok(vec![layout(Val::Int(0), 0)])]);
    assert!(read(&mut disabled, Selection::AvailabilityGroup, 100).await.unwrap().is_empty());
    assert_eq!(disabled.calls.len(), 1);
    for replies in [
        vec![Ok(vec![layout(Val::Null, 0)])],
        vec![Err(ReadError::Stopped(CollectionStop::TimedOut))],
        vec![Ok(vec![layout(Val::Int(1), 1)]), Err(ReadError::Stopped(CollectionStop::TimedOut))],
    ] {
        let count = replies.len();
        let mut reader = FakeRead::new(replies);
        assert!(read(&mut reader, Selection::AvailabilityGroup, 100).await.is_err());
        assert_eq!(reader.calls.len(), count, "no schema/error fallback query");
    }
}
#[tokio::test]
async fn geo_direction_and_primary_only_lag_keep_their_native_meaning() {
    for role in [0, 1, 7] {
        let row = if role == 0 {
            geo()
        } else {
            replace(&geo(), &[("role", Val::Int(role)), ("lag_seconds", Val::Null), ("last_replication_at", Val::Null)])
        };
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        let rows = read(&mut reader, Selection::GeoLinks, 100).await.unwrap();
        assert_eq!(reader.calls.len(), 1);
        assert!(reader.calls[0].sql.contains("CASE WHEN role=0 THEN replication_lag_sec END"));
        let ReplicationFact::GeoLink(raw) = &rows[0] else { panic!("geo") };
        assert_eq!(raw.role, Some(role as u8));
        if role == 7 {
            assert!(application(&rows[0]).is_none());
            continue;
        }
        let app = application(&rows[0]).unwrap();
        assert_eq!(app.role, if role == 0 { ReplicaRole::Replica } else { ReplicaRole::ThisServer });
        assert_eq!(app.lag_seconds, if role == 0 { Some(3.0) } else { None });
        if role == 0 {
            assert_eq!(raw.last_replication_at.as_deref(), Some("2020-01-01T00:00:00.000000000Z"));
        }
    }
}
#[tokio::test]
async fn malformed_rows_duplicates_and_overflow_fail_instead_of_producing_a_clean_replication_state() {
    for (limit, rows) in [
        (0, vec![geo()]),
        (10, vec![geo(), geo()]),
        (10, vec![replace(&geo(), &[("link_id", Val::Text("not-a-guid".into()))])]),
        (10, vec![replace(&geo(), &[("role", Val::Int(1))])]),
        (10, vec![replace(&geo(), &[("lag_seconds", Val::Int(-1))])]),
        (10, vec![replace(&geo(), &[("last_replication_at", Val::Text("2020-01-01T00:00:00".into()))])]),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, Selection::GeoLinks, limit).await.is_err());
    }
    assert_eq!(bytes(&Some(i64::MAX.to_string())), None, "native KB overflow is not a display value");
}

#[test]
fn application_coverage_keeps_failed_reads_and_unknown_roles_distinct_from_known_empty_replication() {
    use culprit_core::SourceState;
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, Ok(vec![])).is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Empty);
    let denied = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied");
    assert!(record_application_result(&mut coverage, Err(denied)).is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    assert!(coverage.outcomes()[0].reason.is_some());
    let known = geo_link(&geo()).unwrap();
    assert_eq!(record_application_result(&mut coverage, Ok(vec![known])).len(), 1);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial, "a later successful part cannot erase failure");
    let mut coverage = Coverage::default();
    let unknown = availability_replica(&replace(&availability(), &[("is_local", Val::Null)])).unwrap();
    assert!(record_application_result(&mut coverage, Ok(vec![unknown])).is_empty());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    assert!(coverage.outcomes()[0].reason.is_some());
}

#[tokio::test]
async fn local_secondary_scope_comes_from_shared_native_facts_without_another_settings_query() {
    let mut settings = Vec::new();
    let unknown = replace(
        &availability(),
        &[("is_local", Val::Null), ("replica_id", Val::Text("22222222-2222-3333-4444-555555555555".into()))],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![unknown, availability()])]);
    let facts = read(&mut reader, Selection::AvailabilityGroup, 100).await.unwrap();
    record_local_role(&mut settings, &facts);
    assert!(settings.is_empty(), "remote replicas and unknown local roles do not establish this connection's role");
    let secondary = replace(&availability(), &[("is_local", Val::Int(1))]);
    let mut reader = FakeRead::new(vec![Ok(vec![layout(Val::Int(1), 1)]), Ok(vec![secondary])]);
    let facts = read(&mut reader, Selection::AvailabilityGroup, 100).await.unwrap();
    record_local_role(&mut settings, &facts);
    record_local_role(&mut settings, &facts);
    assert_eq!(settings[0].value, "readable-secondary");
    assert_eq!(settings.iter().filter(|row| row.name == culprit_core::REPLICA_SETTING).count(), 1);
}
