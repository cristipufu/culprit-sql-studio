use super::identity_tests::complete_replies;
use super::*;
use crate::{
    query_store::{
        Reader,
        replicas::tests::row as replica,
        tests::{definition_row, summary_row},
    },
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{
    SourceState, source_id,
    telemetry::{NativeIdentity, SourceShutdown},
};

fn replies(mode: &str, catalog: Vec<Rec>) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let summary = replace(&summary_row(), &[("replica_group_id", Val::Int(7)), ("is_forced", Val::Null)]);
    let mut replies = complete_replies(definition_row("SELECT @p"), None, summary);
    let Ok(rows) = &mut replies[0] else { unreachable!() };
    rows[0] = replace(&rows[0], &[("replicas", Val::Int(1)), ("actual_state_desc", Val::Text(mode.into()))]);
    replies.insert(1, Ok(catalog.clone()));
    replies.push(Ok(catalog));
    replies
}

#[tokio::test]
async fn selected_secondary_history_keeps_query_plan_lifetime_and_own_original_costs() {
    let catalog = vec![replica(1, 1, None), replica(7, 2, Some("secondary-native-name"))];
    let mut reader = Reader::new(FakeRead::new(replies("READ_CAPTURE_SECONDARY", catalog)));
    let result = from_query_store(
        &mut reader,
        &SnapshotOptions {
            workload_population: Some(NativeIdentity::new("7")),
            window_minutes: 60,
            plans: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let workload = &result.workload;
    assert_eq!(workload.population, Some(NativeIdentity::new("7")));
    let catalog = workload.population_catalog.as_ref().unwrap();
    assert!(catalog.is_valid() && catalog.partitioned && catalog.requires_selection);
    assert_eq!(catalog.populations.len(), 2);
    assert_eq!(workload.source_message, Some(catalog.populations[1].label.clone()));
    assert_eq!(workload.total_executions, 2.0);
    assert_eq!(workload.total_time_ms, 4.0);
    assert_eq!(workload.total_cpu_ms, Some(3.0));
    assert_eq!(workload.queries.len(), 1);
    let query = &workload.queries[0];
    assert_eq!(query.id, "3", "native query IDs remain usable without synthetic population suffixes");
    assert_eq!(query.plans.len(), 1);
    assert_eq!(query.plans[0].id, "4");
    assert_eq!(query.plans[0].is_forced, None, "global forcing does not establish a selected group's forcing state");
    assert_eq!(query.executions, 2.0);
    assert_eq!(query.costs.as_ref().unwrap().failed, Some(1.0));
    assert!(query.variant_family.is_none());
    assert_eq!(result.query_identities["3"], crate::query_store::decode::query_identity(&summary_row()).unwrap());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn readable_history_survives_without_a_local_capture_or_complete_workload_claim() {
    let mut reader = Reader::new(FakeRead::new(replies("READ_ONLY", vec![replica(7, 42, None)])));
    let result = from_query_store(&mut reader, &SnapshotOptions { window_minutes: 60, plans: 0, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(result.workload.population, Some(NativeIdentity::new("7")));
    assert_eq!(result.workload.total_executions, 2.0);
    assert_eq!(result.workload.queries[0].total_time_ms, 4.0);
    let catalog = result.workload.population_catalog.as_ref().unwrap();
    assert!(!catalog.requires_selection);
    let history: serde_json::Value =
        serde_json::from_str(&catalog.populations[0].native_properties["role_history"]).unwrap();
    assert_eq!(history[0]["role_type"], 42, "unknown native roles stay inspectable without choosing by role");
    assert!(history[0]["replica_name"].is_null());
    let outcome =
        result.coverage.outcomes().into_iter().find(|outcome| outcome.source == source_id::QUERY_STORE).unwrap();
    assert_eq!(outcome.state, SourceState::Partial);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
