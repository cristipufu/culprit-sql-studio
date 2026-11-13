use super::*;
use crate::{
    query_store::{decode, definitions::Definition, payload::Payload, window_sql},
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::telemetry::{CollectionStop, SourceShutdown, StatementOrigin};

fn cost_row() -> Rec {
    Rec::of(&[
        ("query_id", Val::Int(3)),
        ("context_settings_id", Val::Int(1)),
        ("object_id", Val::Int(0)),
        ("query_hash", Val::Text("0011223344556677".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("executions", Val::Int(2)),
        ("aborted", Val::Int(0)),
        ("aborted_us", Val::Float(0.0)),
        ("failed", Val::Int(1)),
        ("log_bytes", Val::Float(8.0)),
        ("total_log_bytes", Val::Float(16.0)),
    ])
}
fn expected() -> HashMap<String, Definition> {
    [(
        "3".into(),
        Definition {
            id: "3".into(),
            origin: StatementOrigin::Application,
            query: Some(decode::query_identity(&cost_row()).unwrap()),
            plan: None,
            object_name: None,
            payload: Payload::Text("SELECT 1".into()),
        },
    )]
    .into()
}
fn query() -> culprit_core::QueryStat {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut query = snapshot.workload.unwrap().queries.remove(0);
    query.id = "3".into();
    query.executions = 2.0;
    query.costs = None;
    query
}
fn reader(result: crate::read::ReadResult<Vec<Rec>>) -> Reader<FakeRead> {
    Reader::new(FakeRead::new(vec![
        Ok(vec![Rec::of(&[("tempdb", Val::Int(0)), ("waits", Val::Int(1))])]),
        Ok(vec![window_sql::tests::row()]),
        Ok(vec![Rec::of(&[("log_bytes", Val::Int(1))])]),
        result,
    ]))
}
#[tokio::test]
async fn matching_native_generation_and_execution_count_admit_costs_with_one_statement_log_denominator() {
    let mut reader = reader(Ok(vec![cost_row()]));
    reader.window(60).await.unwrap();
    let mut queries = vec![query()];
    let mut coverage = Coverage::default();
    attach_runtime_costs(&mut reader, 60, &mut queries, &expected(), &mut coverage).await.unwrap();
    let costs = queries[0].costs.as_ref().unwrap();
    assert_eq!(costs.failed, Some(1.0));
    assert_eq!(costs.aborted, Some(0.0));
    assert_eq!(costs.log_share, Some(0.5));
    assert_eq!(reader.queries(), 4, "no independent later global-log denominator query");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn growing_open_interval_counts_withhold_ratios_but_recycling_or_losing_runtime_invalidates_population() {
    for (change, fatal) in [
        (replace(&cost_row(), &[("executions", Val::Int(3))]), false),
        (replace(&cost_row(), &[("executions", Val::Int(1))]), true),
        (replace(&cost_row(), &[("query_initial_compile_at", Val::Text("2020-01-01T06:01:00Z".into()))]), true),
    ] {
        let mut reader = reader(Ok(vec![change]));
        reader.window(60).await.unwrap();
        let mut queries = vec![query()];
        let mut coverage = Coverage::default();
        let outcome = attach_runtime_costs(&mut reader, 60, &mut queries, &expected(), &mut coverage).await;
        if fatal {
            assert_eq!(CollectionStop::from_error(&outcome.unwrap_err()), CollectionStop::SourceChanged);
        } else {
            outcome.unwrap();
            assert!(coverage.outcomes().iter().any(|source| source.source == sources::QUERY_COSTS.name
                && source.state == culprit_core::SourceState::Partial));
        }
        assert!(queries[0].costs.is_none());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut reader = reader(Ok(vec![]));
    reader.window(60).await.unwrap();
    let error =
        attach_runtime_costs(&mut reader, 60, &mut [query()], &expected(), &mut Coverage::default()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn runtime_cost_timeout_is_terminal_while_permission_is_an_independent_source_gap() {
    for terminal in [false, true] {
        let error = if terminal {
            ReadError::Stopped(CollectionStop::TimedOut)
        } else {
            ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture"))
        };
        let mut reader = reader(Err(error));
        reader.window(60).await.unwrap();
        let mut queries = vec![query()];
        let mut coverage = Coverage::default();
        let outcome = attach_runtime_costs(&mut reader, 60, &mut queries, &expected(), &mut coverage).await;
        assert_eq!(outcome.is_err(), terminal);
        assert_eq!(reader.queries(), 4);
        assert!(queries[0].costs.is_none());
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[test]
fn native_runtime_cost_decoder_rejects_impossible_counts_and_log_shares() {
    for row in [
        replace(&cost_row(), &[("aborted", Val::Int(2)), ("failed", Val::Int(1))]),
        replace(&cost_row(), &[("log_bytes", Val::Float(32.0))]),
    ] {
        assert!(crate::query_store::costs::RunCosts::decode(&row).is_err());
    }
}

#[test]
fn unavailable_native_log_measurement_needs_no_global_denominator_read_and_stays_unknown() {
    let sql = crate::query_store::costs::runs_sql(false);
    assert!(!sql.contains("avg_log_bytes_used") && !sql.contains("CROSS JOIN total"));
    let row = replace(&cost_row(), &[("log_bytes", Val::Null), ("total_log_bytes", Val::Null)]);
    let decoded = crate::query_store::costs::RunCosts::decode(&row).unwrap();
    assert!(decoded.log_bytes.is_none() && decoded.total_log_bytes.is_none());
    assert_eq!(decoded.failed, 1);
}
