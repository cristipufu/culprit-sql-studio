use super::*;
use crate::{resources::test_support::FakeRead, value::Val};

fn with_fields(row: &Rec, values: &[(&str, Val)]) -> Rec {
    let mut fields = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    for (name, value) in values {
        if let Some((_, current)) = fields.iter_mut().find(|(field, _)| field == name) {
            *current = value.clone();
        } else {
            fields.push((name, value.clone()));
        }
    }
    Rec::of(&fields)
}
fn profile_row(at: &str) -> Rec {
    with_fields(
        &crate::query_store::window_sql::tests::layout_row(),
        &[
            ("replicas", Val::Int(1)),
            ("culprit_profile_marker", Val::Int(1)),
            ("profile_observed_at", Val::Text(at.into())),
            ("profile_major", Val::Int(16)),
            ("profile_hadr_enabled", Val::Int(0)),
            ("profile_database_id", Val::Int(7)),
            ("profile_database", Val::Text("fixture".into())),
            ("profile_desired_state", Val::Int(2)),
            ("profile_actual_state", Val::Int(2)),
            ("profile_readonly_reason", Val::Int(0)),
            ("profile_performance_permission", Val::Int(1)),
        ],
    )
}
fn trace(status: i64) -> Rec {
    Rec::of(&[
        ("TraceFlag", Val::Int(12606)),
        ("Status", Val::Int(status)),
        ("Global", Val::Int(status)),
        ("Session", Val::Int(0)),
    ])
}
fn profile() -> Vec<Rec> {
    vec![profile_row("2026-01-01T12:00:00Z"), trace(0)]
}
fn profile_replies() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![Ok(vec![profile_row("2026-01-01T12:00:00Z")]), Ok(vec![trace(0)]), Ok(vec![])]
}
fn runtime_row(group: Val) -> Rec {
    with_fields(
        &crate::query_store::tests::summary_row(),
        &[
            ("runtime_stats_id", Val::Int(9_007_199_254_740_993)),
            ("runtime_stats_interval_id", Val::Int(1)),
            ("replica_group_id", group),
            ("count_executions", Val::Int(9_007_199_254_740_993)),
        ],
    )
}
fn original() -> NativeQueryIdentity {
    crate::query_store::decode::query_identity(&runtime_row(Val::Int(0))).unwrap()
}

#[tokio::test]
async fn exactly_seven_dispatches_keep_completed_empty_catalog_and_raw_zero_without_fallback_or_role_guess() {
    let native = original();
    let mut replies = profile_replies();
    replies.push(Ok(vec![runtime_row(Val::Int(0))]));
    replies.extend(profile_replies());
    let mut owner = FakeRead::new(replies);
    let mut points = Observations::default();
    observe(&mut owner, &native, "fixture", &mut points).await.unwrap();
    assert_eq!(owner.calls.len(), 7);
    assert_eq!(
        owner.calls[0].sql, owner.calls[4].sql,
        "the same configuration projection brackets one exact query sample"
    );
    assert_eq!(owner.calls[0].limit, 1);
    assert_eq!(owner.calls[1].limit, 1);
    assert_eq!(owner.calls[2].limit, MAX_CATALOG_ROWS);
    assert_eq!(owner.calls[3].limit, 3);
    assert!(matches!(&owner.calls[3].parameters[0],Parameter::Integer(id) if id.to_string()==native.query_id));
    let value = json!(points);
    assert_eq!(value["before"]["catalog_rows"], 0);
    assert!(value["before"]["catalog"].is_null());
    assert_eq!(value["before"]["trace"]["status"], false);
    assert_eq!(value["runtime"][0]["replica_group_id"], "0");
    assert_eq!(value["runtime"][0]["query"], json!(native));
    assert_eq!(value["runtime"][0]["count_executions"], "9007199254740993");
    assert_eq!(value["runtime"][0]["runtime_stats_id"], "9007199254740993");
    assert_eq!(points.configuration_unchanged(), None, "empty catalog is observed, not proven disabled membership");
    assert!(owner.calls[3].sql.contains("WHERE q.query_id=@P1"));
    assert!(owner.calls[3].sql.contains("SELECT TOP(3) /* culprit */"));
    assert!(owner.calls[1].sql.contains("TRACESTATUS(12606,-1)"));
    assert!(!owner.calls.iter().any(|call| call.sql.contains("TRACEON")
        || call.sql.contains("TRACEOFF")
        || call.sql.contains("ALTER DATABASE")));
}

#[test]
fn null_zero_and_large_native_groups_stay_distinct_and_never_become_role_or_member_names() {
    let native = original();
    for (raw, expected) in [
        (Val::Null, serde_json::Value::Null),
        (Val::Int(0), json!("0")),
        (Val::Int(9_007_199_254_740_993), json!("9007199254740993")),
    ] {
        let value = json!(RuntimeFragment::decode(&runtime_row(raw), &native).unwrap());
        assert_eq!(value["replica_group_id"], expected);
        assert!(value.get("role").is_none() && value.get("member").is_none());
    }
}

#[test]
fn missing_ambiguous_or_invalid_flag_and_missing_metadata_cannot_select_an_off_representative() {
    let row = profile_row("2026-01-01T12:00:00Z");
    for rows in [
        vec![row.clone()],
        vec![row.clone(), trace(0), trace(1)],
        vec![row.clone(), trace(2)],
        vec![row.clone(), row.clone(), trace(0)],
        vec![with_fields(&row, &[("profile_hadr_enabled", Val::Null)]), trace(0)],
    ] {
        let before = NativeProfilePoint::decode(&rows).unwrap();
        assert_eq!(before.same_configuration(&before), None);
    }
    let missing = NativeProfilePoint::decode(&[row]).unwrap();
    let value = json!(missing);
    assert_eq!(value["trace_rows"], 0);
    assert!(value["trace"].is_null(), "missing flag output is not Status0");
}

fn replica(role: i64) -> Rec {
    Rec::of(&[("replica_group_id", Val::Int(7)), ("role_type", Val::Int(role)), ("replica_name", Val::Null)])
}

#[test]
fn only_equal_complete_observations_compare_while_role_or_native_configuration_changes_stay_unknown() {
    let mut before_rows = profile();
    before_rows.push(replica(1));
    let before = NativeProfilePoint::decode(&before_rows).unwrap();
    assert_eq!(before.same_configuration(&before), Some(true));
    let changed_role =
        NativeProfilePoint::decode(&[profile_row("2026-01-01T12:00:01Z"), trace(0), replica(2)]).unwrap();
    assert_eq!(before.same_configuration(&changed_role), None);
    let changed_flag =
        NativeProfilePoint::decode(&[profile_row("2026-01-01T12:00:01Z"), trace(1), replica(1)]).unwrap();
    assert_eq!(before.same_configuration(&changed_flag), None);
    let changed_hadr = NativeProfilePoint::decode(&[
        with_fields(&profile_row("2026-01-01T12:00:01Z"), &[("profile_hadr_enabled", Val::Int(1))]),
        trace(0),
        replica(1),
    ])
    .unwrap();
    assert_eq!(before.same_configuration(&changed_hadr), None);
    let regressed_clock =
        NativeProfilePoint::decode(&[profile_row("2025-12-31T23:59:59Z"), trace(0), replica(1)]).unwrap();
    assert_eq!(before.same_configuration(&regressed_clock), None);
}

#[tokio::test]
async fn runtime_row_overflow_refuses_without_an_after_read_or_an_invented_complete_sample() {
    let mut replies = profile_replies();
    replies.push(Ok((0..4).map(|_| runtime_row(Val::Int(0))).collect()));
    let mut owner = FakeRead::new(replies);
    let mut points = Observations::default();
    let error = observe(&mut owner, &original(), "fixture", &mut points).await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::RowLimit.error().detail);
    assert_eq!(owner.calls.len(), 4);
    assert_eq!(points.runtime_rows, Some(4));
    assert!(points.runtime.is_empty() && points.after.is_none());
}

#[tokio::test]
async fn denied_or_terminal_native_failure_keeps_prior_points_and_never_dispatches_fallback_after_it() {
    let denied = culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "private-native-error-canary");
    let mut owner = FakeRead::new(vec![Err(ReadError::Source(denied.clone()))]);
    let mut points = Observations::default();
    let error = observe(&mut owner, &original(), "fixture", &mut points).await.unwrap_err();
    assert_eq!(error.code, denied.code);
    assert_eq!(owner.calls.len(), 1);
    assert!(points.before.is_none() && points.after.is_none());
    let mut replies = profile_replies();
    replies.push(Err(ReadError::Stopped(CollectionStop::TimedOut)));
    let mut owner = FakeRead::new(replies);
    let mut points = Observations::default();
    let error = observe(&mut owner, &original(), "fixture", &mut points).await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::TimedOut.error().detail);
    assert_eq!(owner.calls.len(), 4);
    assert!(points.before.is_some() && points.after.is_none());
    assert!(!serde_json::to_string(&points).unwrap().contains("private-native-error-canary"));
}

#[tokio::test]
async fn foreign_database_or_changed_query_incarnation_refuses_before_any_later_phase() {
    let wrong = with_fields(&profile_row("2026-01-01T12:00:00Z"), &[("profile_database", Val::Text("foreign".into()))]);
    let mut owner = FakeRead::new(vec![Ok(vec![wrong])]);
    let mut points = Observations::default();
    let error = observe(&mut owner, &original(), "fixture", &mut points).await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::SourceChanged.error().detail);
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(json!(points)["before"]["facts"]["database"], "foreign");
    let recycled = with_fields(
        &runtime_row(Val::Int(0)),
        &[("query_initial_compile_at", Val::Text("2026-01-01T00:00:00Z".into()))],
    );
    let mut replies = profile_replies();
    replies.push(Ok(vec![recycled]));
    let mut owner = FakeRead::new(replies);
    let mut points = Observations::default();
    let error = observe(&mut owner, &original(), "fixture", &mut points).await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::SourceChanged.error().detail);
    assert_eq!(owner.calls.len(), 4);
    assert!(points.after.is_none());
}

#[tokio::test]
async fn cancellation_interrupts_only_the_borrowed_read_and_the_same_owner_is_closed_before_return() {
    let cancellation = CollectionCancellation::default();
    let native = original();
    let mut owner = FakeRead::new(profile_replies()).pause_on_query(4);
    let mut points = Observations::default();
    let result = {
        let work = until_cancelled(&cancellation, observe(&mut owner, &native, "fixture", &mut points));
        tokio::pin!(work);
        tokio::select! {biased;result=&mut work=>panic!("paused native fixture ended unexpectedly: {result:?}"),_ = tokio::task::yield_now()=>()}
        cancellation.cancel();
        work.await
    };
    assert_eq!(result.unwrap_err().detail, CollectionStop::Cancelled.error().detail);
    assert_eq!(owner.calls.len(), 4);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    assert_eq!(owner.closed, 1);
    assert!(points.before.is_some() && points.after.is_none());
}
