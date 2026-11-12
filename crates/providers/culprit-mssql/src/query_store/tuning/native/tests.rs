use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn layout_row() -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Bool(true)),
        ("automatic_tuning", Val::Int(1)),
        ("recommendations", Val::Int(1)),
        ("query_store", Val::Int(1)),
        ("forcing_type", Val::Int(1)),
        ("hints", Val::Int(1)),
        ("hint_replica", Val::Int(1)),
        ("hint_source", Val::Int(1)),
    ])
}
pub(crate) fn option_row() -> Rec {
    Rec::of(&[
        ("name", Val::Text("FORCE_LAST_GOOD_PLAN".into())),
        ("desired_state_desc", Val::Text("ON".into())),
        ("actual_state_desc", Val::Text("OFF".into())),
        ("reason_desc", Val::Text("DISABLED".into())),
    ])
}
pub(crate) fn forced_row() -> Rec {
    Rec::of(&[
        ("query_id", Val::Int(3)),
        ("plan_id", Val::Int(9)),
        ("force_failure_count", Val::Int(9_007_199_254_740_993)),
        ("last_force_failure_reason_desc", Val::Text("NO_PLAN".into())),
        ("last_execution", Val::Text("2020-01-01T00:00:00Z".into())),
        ("forcing", Val::Text("MANUAL".into())),
    ])
}
fn recommendation_row() -> Rec {
    let mut columns: Vec<(&str, Val)> = [
        "reason",
        "state_reason",
        "native_applied",
        "applied_at",
        "index_schema",
        "index_table",
        "index_name",
        "index_columns",
        "included_columns",
        "regressed_cpu_stdev",
        "recommended_cpu_stdev",
    ]
    .into_iter()
    .map(|name| (name, Val::Null))
    .collect();
    columns.extend([
        ("name", Val::Text("PR_3".into())),
        ("type", Val::Text("FORCE_LAST_GOOD_PLAN".into())),
        ("score", Val::Int(90)),
        ("native_valid_since", Val::Text("2020-01-01T00:00:00.1533333".into())),
        ("last_refresh", Val::Text("2020-01-01T01:00:00.1533333".into())),
        ("valid_since", Val::Text("2019-12-31T22:00:00Z".into())),
        ("state", Val::Text("Active".into())),
        ("query_id", Val::Text("3".into())),
        ("regressed_plan_id", Val::Text("9".into())),
        ("recommended_plan_id", Val::Text("8".into())),
        ("regressed_executions", Val::Text("9007199254740993".into())),
        ("recommended_executions", Val::Text("2".into())),
        ("regressed_aborted", Val::Text("9007199254740993".into())),
        ("recommended_aborted", Val::Text("9007199254740992".into())),
        ("regressed_cpu", Val::Text("10.5".into())),
        ("recommended_cpu", Val::Text("2.5".into())),
        ("script", Val::Text("EXEC sys.sp_query_store_force_plan @query_id=3,@plan_id=8".into())),
        ("last_execution", Val::Text("2020-01-01T00:00:00Z".into())),
        ("is_executable_action", Val::Bool(true)),
        ("is_revertable_action", Val::Bool(true)),
    ]);
    Rec::of(&columns)
}
fn layout() -> Layout {
    Layout {
        metadata_visible: true,
        automatic_tuning: true,
        recommendations: true,
        query_store: true,
        forcing_type: true,
        hints: true,
        hint_replica: true,
        hint_source: true,
    }
}

#[tokio::test]
async fn bigint_failures_keep_precision_and_null_or_malformed_values_never_become_zero() {
    let mut reader = FakeRead::new(vec![Ok(vec![forced_row()])]);
    let facts = read_kind(&mut reader, Kind::ForcedPlans, &layout(), 3).await.unwrap().unwrap();
    let TuningFact::ForcedPlan(value) = &facts[0] else { panic!("forced plan") };
    assert_eq!(value.failures, "9007199254740993");
    assert_eq!(value.reset, culprit_core::ResetMarker::Unknown);
    for invalid in [
        replace(&forced_row(), &[("force_failure_count", Val::Null)]),
        replace(&forced_row(), &[("force_failure_count", Val::Int(-1))]),
    ] {
        assert!(read_kind(&mut FakeRead::new(vec![Ok(vec![invalid])]), Kind::ForcedPlans, &layout(), 3).await.is_err());
    }
}
#[tokio::test]
async fn recommendation_time_cpu_and_aborted_counts_drive_the_same_app_projection_without_losing_native_evidence() {
    let mut reader = FakeRead::new(vec![Ok(vec![recommendation_row()])]);
    let facts = read_kind(&mut reader, Kind::Recommendations, &layout(), 3).await.unwrap().unwrap();
    let TuningFact::Recommendation(value) = &facts[0] else { panic!("recommendation") };
    assert_eq!(value.regressed.executions.as_deref(), Some("9007199254740993"));
    assert_eq!(value.regressed.stdev_cpu_us, None);
    assert_eq!(value.valid_since_without_offset.as_deref(), Some("2020-01-01T00:00:00.1533333"));
    assert_eq!(value.valid_since_utc_estimate.as_deref(), Some("2019-12-31T22:00:00.000000000Z"));
    let mut advice = culprit_core::TuningAdvice::default();
    super::super::projection::append_application(&mut advice, facts.into_iter().next().unwrap());
    assert!(advice.recommendations[0].error_prone, "integer comparison must happen before display rounding");
    assert!((advice.recommendations[0].estimated_gain.unwrap() - 8.0 / 10.5).abs() < 1e-12);
    let sql = recommendations_sql();
    assert!(sql.contains("$.planForceDetails.regressedPlanAbortedCount"));
    assert!(!sql.contains("PlanErrorCount"));
    for invalid in [
        replace(&recommendation_row(), &[("regressed_cpu", Val::Text("NaN".into()))]),
        replace(&recommendation_row(), &[("regressed_aborted", Val::Text("1.5".into()))]),
        replace(&recommendation_row(), &[("score", Val::Int(101))]),
    ] {
        assert!(
            read_kind(&mut FakeRead::new(vec![Ok(vec![invalid])]), Kind::Recommendations, &layout(), 3).await.is_err()
        );
    }
}
#[tokio::test]
async fn absent_features_do_not_execute_queries_and_failed_or_oversized_sources_do_not_report_empty_success() {
    let mut missing = layout();
    missing.hints = false;
    let mut reader = FakeRead::new(vec![]);
    assert!(read_kind(&mut reader, Kind::Hints, &missing, 3).await.unwrap().is_none());
    assert!(reader.calls.is_empty());
    missing.metadata_visible = false;
    let error = read_kind(&mut reader, Kind::Hints, &missing, 3).await.err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::PermissionDenied);
    assert!(error.hint.as_ref().is_some_and(|hint| hint.contains("VIEW DEFINITION")));
    assert!(reader.calls.is_empty(), "hidden metadata must not establish feature absence");
    for reply in [Err(ReadError::Stopped(CollectionStop::TimedOut)), Ok(vec![forced_row(), forced_row()])] {
        let mut reader = FakeRead::new(vec![reply]);
        assert!(read_kind(&mut reader, Kind::ForcedPlans, &layout(), 1).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn hints_keep_replica_identity_and_hide_oversized_payloads_before_fetching_them() {
    let hint = "OPTION(USE HINT('fixture-private-canary'))";
    let row = Rec::of(&[
        ("query_hint_id", Val::Int(1)),
        ("query_id", Val::Int(3)),
        ("replica_group_id", Val::Int(1)),
        ("source_bytes", Val::Int(hint.encode_utf16().count() as i64 * 2)),
        ("query_hint_text", Val::Text(hint.into())),
        ("query_hint_failure_count", Val::Int(9_007_199_254_740_993)),
        ("last_query_hint_failure_reason_desc", Val::Null),
        ("native_source", Val::Int(-1)),
        ("native_source_name", Val::Text("USER".into())),
    ]);
    let mut reader = FakeRead::new(vec![Ok(vec![row.clone(), replace(&row, &[("replica_group_id", Val::Int(2))])])]);
    let facts = read_kind(&mut reader, Kind::Hints, &layout(), 3).await.unwrap().unwrap();
    assert_ne!(facts[0].key(), facts[1].key());
    let TuningFact::QueryHint(value) = &facts[0] else { panic!("hint") };
    assert_eq!(value.failures, "9007199254740993");
    assert_eq!(value.native_source, Some(-1), "unknown native enum codes remain facts");
    assert_eq!(value.scope, culprit_core::QueryStoreHintScope::ReplicaGroup { id: "1".into() });
    assert!(matches!(&value.hint,DefinitionPayload::Available{text,..} if text==hint));
    let mut redacted = facts[0].clone();
    redacted.redact(culprit_core::Engine::Mssql);
    assert!(!serde_json::to_string(&redacted).unwrap().contains("fixture-private-canary"));
    let oversized =
        replace(&row, &[("source_bytes", Val::Int(PAYLOAD_NATIVE_BYTES + 2)), ("query_hint_text", Val::Null)]);
    let facts =
        read_kind(&mut FakeRead::new(vec![Ok(vec![oversized])]), Kind::Hints, &layout(), 3).await.unwrap().unwrap();
    assert!(matches!(
        &facts[0],
        TuningFact::QueryHint(NativeQueryHint {
            hint: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized },
            ..
        })
    ));
    assert!(hints_sql(&layout()).contains("DATALENGTH(query_hint_text)<=@P2"));
    let null_scope = replace(&row, &[("replica_group_id", Val::Null)]);
    let facts = read_kind(&mut FakeRead::new(vec![Ok(vec![null_scope.clone()])]), Kind::Hints, &layout(), 3)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&facts[0],TuningFact::QueryHint(value) if value.scope==culprit_core::QueryStoreHintScope::Unknown)
    );
    let mut local_layout = layout();
    local_layout.hint_replica = false;
    let facts = read_kind(&mut FakeRead::new(vec![Ok(vec![null_scope.clone()])]), Kind::Hints, &local_layout, 3)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&facts[0],TuningFact::QueryHint(value) if value.scope==culprit_core::QueryStoreHintScope::Local));
    local_layout.metadata_visible = false;
    let facts = read_kind(&mut FakeRead::new(vec![Ok(vec![null_scope])]), Kind::Hints, &local_layout, 3)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&facts[0],TuningFact::QueryHint(value) if value.scope==culprit_core::QueryStoreHintScope::Unknown)
    );
}

#[tokio::test]
async fn application_tuning_keeps_successful_siblings_and_discards_all_results_if_shutdown_is_unconfirmed() {
    fn replies() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
        vec![
            Ok(vec![layout_row()]),
            Ok(vec![option_row()]),
            Err(ReadError::Source(culprit_core::Error::new(
                culprit_core::ErrorCode::PermissionDenied,
                "fixture denied",
            ))),
            Ok(vec![forced_row()]),
            Ok(vec![]),
        ]
    }
    let collected = super::super::read(&mut FakeRead::new(replies())).await;
    let mut coverage = culprit_core::Coverage::default();
    let advice = collected.finish(&mut coverage, "fixture grant hint", SourceShutdown::Quiescent, true).unwrap();
    assert_eq!(advice.automatic_tuning.len(), 1);
    assert_eq!(advice.forced_plans.len(), 1);
    assert!(advice.recommendations.is_empty());
    assert!(coverage.outcomes().iter().any(|outcome| outcome.source == crate::sources::TUNING_RECOMMENDATIONS.name
        && outcome.state == culprit_core::SourceState::Unavailable));
    assert!(
        coverage.outcomes().iter().any(|outcome| outcome.source == crate::sources::TUNING.name
            && outcome.state == culprit_core::SourceState::Partial)
    );
    let collected = super::super::read(&mut FakeRead::new(replies())).await;
    let mut coverage = culprit_core::Coverage::default();
    assert!(collected.finish(&mut coverage, "fixture grant hint", SourceShutdown::Unconfirmed, true).is_none());
    assert!(
        coverage
            .outcomes()
            .iter()
            .filter(|outcome| outcome.source != crate::sources::TUNING.name)
            .all(|outcome| outcome.state == culprit_core::SourceState::Unavailable)
    );
}
