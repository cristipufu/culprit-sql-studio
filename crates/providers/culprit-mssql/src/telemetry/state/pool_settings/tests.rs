use super::*;
use crate::{
    read::{ReadError, ReadResult, ReadSession},
    resources::{
        pool::test_support::{settings_layout, settings_member, settings_point},
        test_support::{FakeRead, replace},
    },
    telemetry::state::{
        StateSource,
        tests::{clock, pressure},
    },
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, SourceState};
const MEMBER: &str = "2020-01-01T06:00:00Z";
const FROM: &str = "2020-01-01T06:00:01Z";
const POINT: &str = "2020-01-01T06:00:02Z";
const UNTIL: &str = "2020-01-01T06:00:03Z";
const FINAL: &str = "2020-01-01T06:00:04Z";
fn target() -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: culprit_core::Engine::Mssql,
        kind: SharedResourceKind::Pool,
        namespace: NativeIdentity::new("fixture-server").qualified("kind", "azure-sql-logical-server"),
        native: NativeIdentity::new("fixture-pool"),
    }
}
fn request(total: u32) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared { resource: target() },
        selection: CollectionSelection::ResourcePoolSettings,
        budget: CollectionBudget { max_queries: total, ..Default::default() },
    }
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![settings_member(MEMBER)]),
        Ok(vec![clock(false, FROM)]),
        Ok(vec![replace(
            &pressure(false),
            &[("observed_at", Val::Text(FROM.into())), ("sample_at", Val::Text(MEMBER.into()))],
        )]),
        Ok(settings_layout()),
        Ok(vec![settings_point(POINT)]),
        Ok(vec![clock(false, UNTIL)]),
        Ok(vec![settings_member(FINAL)]),
    ]
}
fn source_from_replies(rows: Vec<ReadResult<Vec<Rec>>>, request: &CollectionRequest) -> StateSource<FakeRead> {
    let budget = ReadSession::budget_after_setup(bounded_budget(&request.budget)).unwrap();
    StateSource::with_io(
        FakeRead::new(rows).with_query_limit(budget.max_queries),
        request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<FakeRead>) -> Vec<TelemetryRecord> {
    let mut rows = vec![];
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::SettingObservations, cursor.as_deref()).await.unwrap()
        else {
            panic!("bounded Settings page")
        };
        rows.extend(records);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    rows
}
#[tokio::test]
async fn pool_settings_reuse_state_owner_with_nine_total_dispatches_original_header_and_no_database_projection() {
    let mut source = source_from_replies(replies(), &request(9));
    assert_eq!(source.query_count(), 0);
    let original = source.describe().await.unwrap();
    assert!(original.partition_is_consistent());
    assert_eq!(original.partition, SourcePartition::Target);
    assert_eq!(original.target, CollectionTarget::Shared(target()));
    let membership = mssql_pool_settings_membership(&original).unwrap().unwrap();
    assert_eq!(membership.member_observed_at, "2020-01-01T06:00:00.000000000Z");
    assert_eq!(membership.point.as_ref().unwrap().observed_at, "2020-01-01T06:00:02.000000000Z");
    assert_eq!(membership.point.as_ref().unwrap().database_id, 7);
    let rows = drain(&mut source).await;
    assert_eq!(rows.len(), 6);
    for row in rows {
        let TelemetryRecord::Setting(value) = &row else { panic!() };
        assert!(value.fits_source(&original));
        assert!(crate::settings::application(value).is_none(), "a pool configuration cannot grade the routed database");
        ExportedRecord::new(
            &original,
            &CaptureIdentity { observation_id: "pool-original".into(), revision: None },
            row,
        )
        .unwrap();
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 8, "one independently counted SET makes total9");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn denied_hidden_or_unknown_layout_keeps_original_membership_and_explicit_unavailable_not_healthy_empty() {
    for denied in [false, true] {
        let mut rows = replies();
        rows[4] = if denied {
            Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "owned denial")))
        } else {
            Ok(vec![])
        };
        rows.remove(5);
        let mut source = source_from_replies(rows, &request(9));
        let original = source.describe().await.unwrap();
        let membership = mssql_pool_settings_membership(&original).unwrap().unwrap();
        assert!(membership.point.is_none());
        let native = original.observation.snapshot().unwrap();
        assert_eq!(native.records, 0);
        assert_eq!(native.sources[0].state, SourceState::Unavailable);
        assert_eq!(
            native.sources[0].error_code,
            Some(if denied { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
        );
        assert!(!native.sources[0].complete_scope);
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn caller_total_eight_cannot_borrow_the_setup_slot_or_skip_final_native_membership() {
    for total in [8, 9] {
        let mut source = source_from_replies(replies(), &request(total));
        source.describe().await.unwrap();
        drain(&mut source).await;
        if total == 8 {
            assert_eq!(CollectionStop::from_error(&source.finish().await.err().unwrap()), CollectionStop::QueryLimit);
            assert_eq!(source.query_count(), 7);
        } else {
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), 8);
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn final_member_server_pool_guids_role_and_backward_clock_cannot_complete_the_original_point() {
    for (name, value) in [
        ("server_name", Val::Text("foreign-server".into())),
        ("pool_name", Val::Text("foreign-pool".into())),
        ("incarnation", Val::Text("33333333-3333-3333-3333-333333333333".into())),
        ("physical_incarnation", Val::Text("44444444-4444-4444-4444-444444444444".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text(POINT.into())),
    ] {
        let mut rows = replies();
        rows[7] = Ok(vec![replace(&settings_member(FINAL), &[(name, value)])]);
        let mut source = source_from_replies(rows, &request(9));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{name}");
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    for at in [UNTIL, FINAL] {
        let mut rows = replies();
        rows[7] = Ok(vec![settings_member(at)]);
        let mut source = source_from_replies(rows, &request(9));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn current_row_clock_outside_original_capture_or_changed_selected_database_stops_before_publication() {
    for change in ["clock", "database"] {
        let mut rows = replies();
        rows[5] = Ok(vec![if change == "clock" {
            settings_point(MEMBER)
        } else {
            replace(&settings_point(POINT), &[("selected_database_id", Val::Int(8))])
        }]);
        let mut source = source_from_replies(rows, &request(9));
        assert!(source.describe().await.is_err());
        assert!(source.query_count() <= 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn unrelated_subjects_unpooled_members_and_foreign_platforms_never_receive_the_pool_family() {
    let mut req = request(9);
    let CollectionSubject::Shared { resource } = &mut req.subject else { panic!() };
    resource.kind = SharedResourceKind::Instance;
    assert!(StateSource::with_io(FakeRead::new(vec![]), &req, "fixture.invalid".into(), 1433).is_err());
    for unpooled in [false, true] {
        let mut rows = replies();
        if unpooled {
            rows[1] = Ok(vec![replace(&settings_member(MEMBER), &[("pool_name", Val::Null)])]);
        } else {
            rows[0] = Ok(vec![Rec::of(&[("edition", Val::Int(3))])]);
        }
        let mut source = source_from_replies(rows, &request(9));
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), if unpooled { 2 } else { 1 });
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn native_cancellation_debt_stops_the_family_and_preserves_unconfirmed_close_without_finish_work() {
    let mut rows = replies();
    rows[5] = Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
    let mut source = source_from_replies(rows, &request(9));
    source.io.shutdown = SourceShutdown::Unconfirmed;
    assert_eq!(
        CollectionStop::from_error(&source.describe().await.err().unwrap()),
        CollectionStop::CancellationUnconfirmed
    );
    assert_eq!(source.query_count(), 6);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}

fn database_replies(pooled: bool) -> Vec<ReadResult<Vec<Rec>>> {
    let mut rows = replies();
    if !pooled {
        for index in [1, 5, 7] {
            let values = rows[index].as_mut().unwrap();
            values[0] = replace(&values[0], &[("pool_name", Val::Null)]);
        }
    }
    rows
}
fn database_request(total: u32) -> CollectionRequest {
    CollectionRequest { subject: CollectionSubject::Database, ..request(total) }
}
#[tokio::test]
async fn single_and_pooled_database_originals_use_the_same_six_field_reader_and_total_nine_budget() {
    for pooled in [false, true] {
        let mut source = source_from_replies(database_replies(pooled), &database_request(9));
        let original = source.describe().await.unwrap();
        let CollectionTarget::Database(database) = &original.target else { panic!() };
        assert_eq!(database.database, "fixture-db");
        assert_eq!(database.incarnation.as_deref(), Some("11111111-1111-1111-1111-111111111111"));
        let membership = mssql_pool_settings_membership(&original).unwrap().unwrap();
        assert_eq!(&membership.database, database);
        assert_eq!(NativeReadContext::from_snapshot(&original).unwrap().database_writable, Some(true));
        assert_eq!(membership.elastic_pool.is_some(), pooled);
        assert_eq!(drain(&mut source).await.len(), 6);
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 8, "SET consumes the ninth caller slot");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut source = source_from_replies(database_replies(false), &database_request(8));
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.err().unwrap()), CollectionStop::QueryLimit);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn unpooled_database_final_name_guid_role_membership_and_clock_changes_refuse_the_original() {
    for (name, value) in [
        ("database_name", Val::Text("foreign-db".into())),
        ("incarnation", Val::Text("33333333-3333-3333-3333-333333333333".into())),
        ("physical_incarnation", Val::Text("44444444-4444-4444-4444-444444444444".into())),
        ("pool_name", Val::Text("new-membership".into())),
        ("elastic_pool_visibility", Val::Int(2)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text(POINT.into())),
    ] {
        let mut rows = database_replies(false);
        let final_row = rows[7].as_mut().unwrap();
        final_row[0] = replace(&final_row[0], &[(name, value)]);
        let mut source = source_from_replies(rows, &database_request(9));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{name}");
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn unavailable_single_database_settings_keep_original_context_without_inventing_an_elastic_pool() {
    for denied in [false, true] {
        let mut rows = database_replies(false);
        rows[4] = if denied {
            Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "owned denial")))
        } else {
            Ok(vec![])
        };
        rows.remove(5);
        let mut source = source_from_replies(rows, &database_request(9));
        let original = source.describe().await.unwrap();
        let membership = mssql_pool_settings_membership(&original).unwrap().unwrap();
        assert!(membership.elastic_pool.is_none() && membership.point.is_none());
        assert_eq!(original.observation.snapshot().unwrap().sources[0].state, SourceState::Unavailable);
        assert!(drain(&mut source).await.is_empty());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 7);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn optional_elastic_metadata_failure_does_not_hide_database_settings_or_authenticate_a_shared_pool() {
    for (visibility, expected) in [(1, ErrorCode::Unsupported), (2, ErrorCode::PermissionDenied)] {
        let mut rows = database_replies(false);
        for index in [1, 5, 7] {
            let values = rows[index].as_mut().unwrap();
            values[0] = replace(&values[0], &[("elastic_pool_visibility", Val::Int(visibility))]);
        }
        let shared_rows = rows.iter().map(|row| Ok(row.as_ref().unwrap().clone())).collect();
        let mut source = source_from_replies(rows, &database_request(9));
        let original = source.describe().await.unwrap();
        let membership = mssql_pool_settings_membership(&original).unwrap().unwrap();
        assert!(membership.elastic_pool.is_none());
        assert_eq!(membership.elastic_pool_error, Some(expected));
        assert_eq!(drain(&mut source).await.len(), 6);
        assert_eq!(original.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        let mut shared = source_from_replies(shared_rows, &request(9));
        assert!(shared.describe().await.is_err());
        assert_eq!(shared.query_count(), 2);
        assert_eq!(shared.close().await, SourceShutdown::Quiescent);
    }
}
