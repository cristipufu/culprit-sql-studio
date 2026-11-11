use super::*;
use crate::{
    read::{Parameter, ReadError},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::telemetry::*;
fn summary_row() -> Rec {
    Rec::of(&[
        ("query_id", Val::Int(3)),
        ("plan_id", Val::Int(4)),
        ("executions", Val::Int(2)),
        ("duration_us", Val::Float(4000.0)),
        ("cpu_us", Val::Float(3000.0)),
        ("logical_reads", Val::Float(10.0)),
        ("physical_reads", Val::Float(0.0)),
        ("row_total", Val::Float(2.0)),
        ("tempdb_pages", Val::Null),
        ("sum_sq_us", Val::Float(10_000_000.0)),
        ("min_us", Val::Int(1000)),
        ("max_us", Val::Int(3000)),
        ("first_seen", Val::Text("2020-01-01T06:00:00.1533333Z".into())),
        ("last_seen", Val::Text("2020-01-01T06:00:01.3066667Z".into())),
        ("is_forced", Val::Int(0)),
    ])
}
fn runtime_row(duration: i64) -> Rec {
    Rec::of(&[
        ("row_id", Val::Int(duration)),
        ("plan_id", Val::Int(4)),
        ("replica_group_id", Val::Int(7)),
        ("execution_type", Val::Int(3)),
        ("count_executions", Val::Int(1)),
        ("avg_duration", Val::Float(duration as f64)),
        ("stdev_duration", Val::Float(0.0)),
        ("min_duration", Val::Int(duration)),
        ("max_duration", Val::Int(duration)),
        ("avg_cpu_time", Val::Float(1500.0)),
        ("avg_logical_io_reads", Val::Float(5.0)),
        ("avg_physical_io_reads", Val::Float(0.0)),
        ("avg_logical_io_writes", Val::Null),
        ("avg_rowcount", Val::Float(1.0)),
        ("avg_dop", Val::Float(1.0)),
        ("avg_query_max_used_memory", Val::Null),
        ("avg_clr_time", Val::Null),
        ("avg_log_bytes_used", Val::Null),
        ("avg_tempdb_space_used", Val::Null),
        ("first_execution_at", Val::Text("2020-01-01T06:00:00.1533333Z".into())),
        ("last_execution_at", Val::Text("2020-01-01T06:00:01.3066667Z".into())),
    ])
}
#[tokio::test]
async fn raw_fragments_and_database_aggregates_preserve_native_units_and_exact_counts() {
    let mut reader =
        Reader::new(FakeRead::new(vec![Ok(vec![Rec::of(&[("tempdb", Val::Int(0))])]), Ok(vec![summary_row()])]));
    let summary = reader.summary(60).await.unwrap().remove(0);
    let records = [runtime_row(1000), runtime_row(3000)]
        .map(|row| decode::record(&row, TelemetryStream::QueryRuntime, StatementReferences::default()).unwrap());
    let duration = records
        .iter()
        .map(|record| match record {
            TelemetryRecord::QueryRuntime(row) => {
                row.metric(&RuntimeMetricKey::Duration).unwrap().average * row.executions.parse::<f64>().unwrap()
            }
            _ => panic!("runtime"),
        })
        .sum::<f64>();
    assert_eq!(summary.moments.duration_us, duration);
    assert_eq!(runtime::milliseconds(duration), 4.0);
    assert_eq!(summary.moments.executions, 2);
    assert_eq!(summary.moments.tempdb_pages, None);
    assert_eq!(summary.first_seen.as_deref(), Some("2020-01-01T06:00:00.153333300Z"));
    let exact = 9_007_199_254_740_993i64;
    let exact = replace(&runtime_row(1000), &[("count_executions", Val::Int(exact))]);
    let TelemetryRecord::QueryRuntime(row) =
        decode::record(&exact, TelemetryStream::QueryRuntime, StatementReferences::default()).unwrap()
    else {
        panic!("runtime")
    };
    assert_eq!(row.executions, "9007199254740993");
    assert_eq!(row.key.native.qualifiers["execution_type"], "3");
    assert_eq!(row.key.native.qualifiers["replica_group_id"], "7");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[test]
fn unknown_and_invalid_native_fields_never_become_measured_zero() {
    assert!(runtime::PlanSummary::decode(&replace(&summary_row(), &[("duration_us", Val::Null)])).is_err());
    assert!(runtime::PlanSummary::decode(&replace(&summary_row(), &[("duration_us", Val::Float(f64::NAN))])).is_err());
    assert!(
        decode::record(
            &replace(&runtime_row(1000), &[("avg_cpu_time", Val::Text("0".into()))]),
            TelemetryStream::QueryRuntime,
            StatementReferences::default()
        )
        .is_err()
    );
}
fn definition_row(sql: &str) -> Rec {
    Rec::of(&[
        ("id", Val::Int(3)),
        ("query_id", Val::Int(3)),
        ("context_settings_id", Val::Int(5)),
        ("object_id", Val::Int(0)),
        ("query_hash", Val::Text("0123456789ABCDEF".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:00Z".into())),
        ("source_bytes", Val::Int((sql.encode_utf16().count() * 2) as i64)),
        ("encrypted", Val::Bool(false)),
        ("restricted", Val::Bool(false)),
        ("payload", Val::Text(sql.into())),
    ])
}
#[tokio::test]
async fn definitions_read_complete_bounded_native_text_and_keep_private_literals_until_export() {
    let sql = "SELECT N'private-definition-canary'";
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![Rec::of(&[("id", Val::Int(3)), ("source_bytes", Val::Int((sql.encode_utf16().count() * 2) as i64))])]),
        Ok(vec![definition_row(sql)]),
    ]));
    let definition = reader.definitions(&["3".into()], definitions::Kind::Query, 1024).await.unwrap().remove(0);
    assert_eq!(definition.payload.text(), Some(sql));
    assert_eq!(definition.query.unwrap().query_id, "3");
    let redacted = DefinitionPayload::sql(definition.payload.text().unwrap(), culprit_core::Engine::Mssql);
    let DefinitionPayload::Available { text, .. } = redacted else { panic!("safe SQL") };
    assert!(!text.contains("private-definition-canary"));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn missing_or_changed_definition_identity_cannot_supply_text_for_a_different_query() {
    let sql = "SELECT 1";
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![Rec::of(&[("id", Val::Int(3)), ("source_bytes", Val::Int(16))])]),
        Ok(vec![replace(&definition_row(sql), &[("query_id", Val::Int(4))])]),
    ]));
    let error = reader.definitions(&["3".into()], definitions::Kind::Query, 1024).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
}
#[tokio::test]
async fn source_budget_and_cancellation_errors_are_not_retried_as_schema_fallbacks() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![Rec::of(&[("tempdb", Val::Int(1))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ]));
    let error = reader.summary(60).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let Parameter::Text(ids) = reader::ids_parameter(&["3".into(), "4".into()]).unwrap() else {
        panic!("XML selectors")
    };
    assert_eq!(ids, "<ids><id id=\"3\"/><id id=\"4\"/></ids>");
    assert!(reader::ids_parameter(&["3);DROP TABLE x".into()]).is_err());
}

#[test]
fn negative_native_wait_totals_are_rejected_instead_of_wrapping_into_a_large_measurement() {
    let row = Rec::of(&[("wait_category_desc", Val::Text("Lock".into())), ("wait_ms", Val::Int(-1))]);
    assert!(waits::Summary::decode(&row, false).is_err());
}

#[tokio::test]
async fn cost_timeout_is_not_retried_as_an_older_database_layout() {
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![Rec::of(&[("log_bytes", Val::Int(1))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ]));
    let error = reader.costs(60, &["3".into()]).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
}

#[test]
fn optional_runtime_costs_preserve_nulls_and_reject_invalid_values() {
    let TelemetryRecord::QueryRuntime(record) =
        decode::record(&runtime_row(1000), TelemetryStream::QueryRuntime, StatementReferences::default()).unwrap()
    else {
        panic!("runtime")
    };
    assert_eq!(record.metric(&RuntimeMetricKey::Rows).map(|metric| metric.average), Some(1.0));
    assert_eq!(record.metric(&RuntimeMetricKey::Memory), None);
    assert_eq!(record.metric(&RuntimeMetricKey::LogBytes), None);
    assert!(
        decode::record(
            &replace(&runtime_row(1000), &[("avg_tempdb_space_used", Val::Float(-1.0))]),
            TelemetryStream::QueryRuntime,
            StatementReferences::default()
        )
        .is_err()
    );
    assert!(
        decode::record(
            &replace(&runtime_row(1000), &[("avg_dop", Val::Float(f64::INFINITY))]),
            TelemetryStream::QueryRuntime,
            StatementReferences::default()
        )
        .is_err()
    );
}

pub(crate) fn compilation_row() -> Rec {
    Rec::of(&[
        ("query_id", Val::Int(3)),
        ("count_compiles", Val::Int(9_007_199_254_740_993)),
        ("avg_compile_duration", Val::Float(1500.5)),
        ("first_compile", Val::Text("2020-01-01T06:00:00.1533333Z".into())),
        ("last_compile", Val::Text("2020-01-01T06:01:00Z".into())),
        ("compiling_s", Val::Int(60)),
    ])
}
#[test]
fn compilation_lifetime_is_exact_and_never_borrows_the_runtime_window() {
    let native = costs::Compilation::decode(&compilation_row()).unwrap();
    let exported = native.observation();
    assert_eq!(exported.compilations, "9007199254740993");
    assert_eq!(exported.average_duration_us, 1500.5);
    assert_eq!(exported.first_compile_at, "2020-01-01T06:00:00.153333300Z");
    assert_eq!(native.total_ms(), native.avg_duration_us * native.count as f64 / 1000.0);
    assert!(native.within_window(1));
    let skewed = costs::Compilation::decode(&replace(&compilation_row(), &[("compiling_s", Val::Int(-1))])).unwrap();
    assert!(skewed.observation().is_valid());
    assert!(!skewed.within_window(60), "a future initial compile cannot establish in-window totals");
    for (column, value) in [
        ("count_compiles", Val::Null),
        ("count_compiles", Val::Int(-1)),
        ("avg_compile_duration", Val::Float(f64::NAN)),
        ("first_compile", Val::Text("2020-01-01T06:00:00".into())),
    ] {
        assert!(costs::Compilation::decode(&replace(&compilation_row(), &[(column, value)])).is_err());
    }
}

#[test]
fn native_definition_times_are_canonical_before_interactive_or_export_consumers_see_them() {
    let first = definition_row("SELECT 1");
    let equivalent = replace(&first, &[("query_initial_compile_at", Val::Text("2020-01-01T08:00:00+02:00".into()))]);
    assert_eq!(decode::query_identity(&first).unwrap(), decode::query_identity(&equivalent).unwrap());
    let malformed = replace(&first, &[("query_initial_compile_at", Val::Text("not-a-time".into()))]);
    assert!(decode::query_identity(&malformed).is_err());
}
