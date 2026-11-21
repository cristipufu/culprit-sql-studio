//! Owned native experiments reuse the production reader and never approve a production recipe.
use super::*;
use crate::{
    read::measurement::{MeasuredRead, assert_no_followup, shutdown_state},
    test_support::{OwnedSourceFixture, receipt_directory, save_cost_receipt},
};
use culprit_read::ObservationReader;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

mod cancellation;
mod capture;

const EXPANDED_AG_FROM: &str = "FROM sys.dm_hadr_database_replica_states drs";
const MAX_SOURCE_QUERIES: u32 = 16;

fn budget() -> CollectionBudget {
    bounded_budget(&CollectionBudget {
        max_snapshot_rows: 1024,
        page_rows: 16,
        max_queries: MAX_SOURCE_QUERIES,
        statement_timeout_ms: 3000,
        total_timeout_ms: 15_000,
        ..CollectionBudget::default()
    })
}

fn request(budget: CollectionBudget) -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Replication },
        budget,
    }
}

fn recipe() -> RecipeDescriptor {
    let recipe = crate::telemetry::recipes()
        .into_iter()
        .find(|recipe| recipe.id == "mssql.replication")
        .expect("selected source-owned replication recipe");
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    recipe
}

fn expected_ag() -> bool {
    match std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_EXPANDED_AG").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => panic!("explicit replication fixture expectation must be 0 or 1"),
    }
}

fn expanded_template(accounting: &Value) -> Option<&str> {
    let tasks = accounting["source_read_metrics"]["templates"].as_array()?;
    accounting["phases"].as_array()?.iter().find_map(|phase| {
        let sql = phase["original_sql"].as_str()?;
        let id = phase["template_id"].as_str()?;
        let admitted = tasks.iter().any(|task| {
            task["template_id"].as_str() == Some(id) && task["completed_tasks"].as_u64().is_some_and(|count| count > 0)
        });
        (admitted && sql.contains(EXPANDED_AG_FROM) && id == digest(sql.as_bytes())).then_some(id)
    })
}

#[tokio::test]
#[ignore = "requires a nonce-owned loopback smoke database and private native-cost receipt directory"]
async fn owned_replication_profile_measures_the_actual_reader_scope_limits_and_settlement() {
    let directory = receipt_directory();
    let fixture = tokio::time::timeout(Duration::from_secs(10), OwnedSourceFixture::open("culprit-replication-cost"))
        .await
        .expect("owned fixture admission deadline")
        .expect("owned fixture admission");
    let expected_ag = expected_ag();
    let tested_budget = budget();
    let baseline = capture::capture(&fixture, &tested_budget).await.expect("measured production reader");
    let major = baseline.accounting["preparation"]["native_major"].as_u64().expect("native major");
    let prefix = format!("replication-{major}-ag-{}", u8::from(expected_ag));
    let mut receipt = baseline.receipt("complete-source", &tested_budget, expected_ag);
    receipt["fixture_admission_client_elapsed_ms"] = json!(fixture.admission_elapsed_ms);
    save_cost_receipt(&directory, &format!("{prefix}-complete"), &receipt).unwrap();
    baseline.assert_complete(&fixture, &tested_budget);
    assert_populated_topology(&baseline);
    assert_eq!(
        expanded_template(&baseline.accounting).is_some(),
        expected_ag,
        "HADR-disabled layout-only work cannot qualify the expanded AG SELECT"
    );

    let query_budget = CollectionBudget { max_queries: 1, ..tested_budget.clone() };
    let limited = capture::capture(&fixture, &query_budget).await.expect("query-limit source attempt");
    let mut receipt = limited.receipt("caller-query-limit", &query_budget, expected_ag);
    if limited.shutdown == SourceShutdown::Unconfirmed {
        receipt["remaining_cases_not_run"] = json!(["caller-row-bound", "actual-expanded-ag-cancellation"]);
        receipt["remaining_cases_reason"] = json!("original source shutdown unconfirmed; no replacement work");
    }
    save_cost_receipt(&directory, &format!("{prefix}-query-limit"), &receipt).unwrap();
    assert_eq!(limited.observation.as_ref().err().map(CollectionStop::from_error), Some(CollectionStop::QueryLimit));
    assert_eq!(limited.queries, 1);
    assert!(limited.verification.is_none());
    if limited.shutdown == SourceShutdown::Unconfirmed {
        return;
    }

    let row_budget = CollectionBudget { max_snapshot_rows: 1, page_rows: 1, ..tested_budget.clone() };
    let limited = capture::capture(&fixture, &row_budget).await.expect("row-bound source attempt");
    let mut receipt = limited.receipt("caller-row-bound", &row_budget, expected_ag);
    receipt["row_overflow_observed"] = json!(
        limited
            .observation
            .as_ref()
            .err()
            .is_some_and(|error| CollectionStop::from_error(error) == CollectionStop::RowLimit)
    );
    if limited.shutdown == SourceShutdown::Unconfirmed {
        receipt["remaining_cases_not_run"] = json!(["actual-expanded-ag-cancellation"]);
        receipt["remaining_cases_reason"] = json!("original source shutdown unconfirmed; no replacement work");
    }
    save_cost_receipt(&directory, &format!("{prefix}-row-bound"), &receipt).unwrap();
    if limited.observation.is_ok() {
        limited.assert_complete(&fixture, &row_budget);
    } else {
        assert_eq!(limited.observation.as_ref().err().map(CollectionStop::from_error), Some(CollectionStop::RowLimit));
    }
    if limited.shutdown == SourceShutdown::Unconfirmed {
        return;
    }

    let cancellation = cancellation::observe(&fixture, expanded_template(&baseline.accounting), &tested_budget)
        .await
        .expect("actual source cancellation observation");
    save_cost_receipt(&directory, &format!("{prefix}-cancellation"), &cancellation).unwrap();
    if cancellation["shutdown"] == json!("quiescent") {
        assert_eq!(cancellation["independently_observed_quiet"], json!(true), "{cancellation}");
    }
    assert_eq!(recipe().cost, RecipeCostQualification::Unqualified);
}

fn assert_populated_topology(capture: &capture::Capture) {
    let role = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").ok();
    let group = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP").ok();
    let rows = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_ROWS").ok();
    let (Some(role), Some(group), Some(rows)) = (role.as_deref(), group.as_deref(), rows.as_deref()) else {
        assert!(role.is_none() && group.is_none() && rows.is_none(), "complete topology expectation or none");
        return;
    };
    assert!(matches!(role, "primary" | "secondary"));
    let rows = rows.parse::<usize>().expect("explicit expected native AG row count");
    assert!(rows > 0);
    let observed = capture.observation.as_ref().expect("completed populated AG observation");
    assert_eq!(observed.records.len(), rows);
    let primary = role == "primary";
    assert_eq!(capture.scope.database_writable, Some(primary), "retain the original member's native database role");
    let header = observed.description.observation.snapshot().expect("native AG snapshot");
    let [coverage] = header.sources.as_slice() else { panic!("one native AG source") };
    assert_eq!(coverage.complete_scope, primary, "secondary visibility cannot establish complete partner inventory");
    assert_eq!(
        coverage.state,
        if primary { culprit_core::SourceState::Collected } else { culprit_core::SourceState::Partial }
    );
    assert_eq!(coverage.error_code, None);
    assert_eq!(coverage.stopped, None);
    let mut local = 0;
    for record in &observed.records {
        let TelemetryRecord::Replication(point) = record else { panic!("replication fact") };
        let ReplicationFact::AvailabilityGroup(sample) = &point.fact else { panic!("native AG fact") };
        assert_eq!(sample.group.as_ref().and_then(|group| group.name.as_deref()), Some(group));
        if sample.is_local == Some(true) {
            local += 1;
            assert_eq!(sample.progress.is_primary, Some(primary));
        }
    }
    assert_eq!(local, 1, "one independently identified local member in each native observation");
}

#[test]
fn mentioning_the_view_in_layout_metadata_is_not_execution_of_the_expanded_select() {
    let sql = "SELECT OBJECT_ID(N'sys.dm_hadr_database_replica_states') AS native_object";
    let metadata = json!({
        "phases":[{"template_id":digest(sql.as_bytes()),"original_sql":sql}],
        "source_read_metrics":{"templates":[{"template_id":digest(sql.as_bytes()),"completed_tasks":1}]},
    });
    assert!(expanded_template(&metadata).is_none());
    let sql = format!("SELECT drs.database_id {EXPANDED_AG_FROM} WHERE drs.database_id=DB_ID()");
    let mut actual = json!({
        "phases":[{"template_id":digest(sql.as_bytes()),"original_sql":sql}],
        "source_read_metrics":{"templates":[{"template_id":digest(sql.as_bytes()),"completed_tasks":1}]},
    });
    assert!(expanded_template(&actual).is_some());
    actual["source_read_metrics"]["templates"][0]["completed_tasks"] = json!(0);
    assert!(expanded_template(&actual).is_none(), "a budget-rejected phase did not execute native SQL");
    actual["source_read_metrics"]["templates"][0]["completed_tasks"] = json!(1);
    actual["phases"][0]["template_id"] = json!("0".repeat(64));
    actual["source_read_metrics"]["templates"][0]["template_id"] = json!("0".repeat(64));
    assert!(expanded_template(&actual).is_none(), "recorded SQL and template digest must agree");
}

#[test]
fn cancellation_of_another_or_completed_template_does_not_qualify_the_replication_template() {
    use cancellation::classification;
    assert_eq!(classification(false, true, 1, 0, SourceShutdown::Quiescent, true), "unverified");
    assert_eq!(classification(true, true, 0, 0, SourceShutdown::Quiescent, true), "unverified");
    assert_eq!(classification(true, false, 1, 0, SourceShutdown::Quiescent, true), "unverified");
    assert_eq!(classification(true, true, 1, 0, SourceShutdown::Quiescent, true), "acknowledged");
    assert_eq!(classification(true, true, 1, 0, SourceShutdown::Unconfirmed, true), "unconfirmed");
    assert_eq!(classification(true, true, 0, 1, SourceShutdown::Quiescent, true), "unconfirmed");
    assert_eq!(classification(true, true, 1, 0, SourceShutdown::Quiescent, false), "unverified");
}
