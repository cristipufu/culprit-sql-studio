use super::*;
use crate::{
    query_store::{
        Reader, decode,
        runtime::PlanSummary,
        tests::{runtime_row, summary_row},
        window_sql::{
            WindowVerification,
            tests::{azure_row, layout_row},
        },
    },
    read::NativeRead,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::telemetry::{CollectionStop, SourceShutdown, StatementReferences, TelemetryRecord, TelemetryStream};

fn native(row: &Rec) -> Rec {
    replace(
        row,
        &[
            ("page_server_read_availability", Val::Int(1)),
            ("avg_page_server_io_reads", Val::Float(12.5)),
            ("last_page_server_io_reads", Val::Int(9_007_199_254_740_993)),
            ("min_page_server_io_reads", Val::Int(0)),
            ("max_page_server_io_reads", Val::Int(i64::MAX)),
            ("stdev_page_server_io_reads", Val::Float(1.25)),
        ],
    )
}

fn single_fragment_summary() -> Rec {
    native(&replace(
        &summary_row(),
        &[
            ("page_server_native_fragments", Val::Int(1)),
            ("page_server_native_row", Val::Int(17)),
            ("page_server_native_interval", Val::Int(1)),
            ("page_server_native_outcome", Val::Int(0)),
            ("failed", Val::Int(0)),
        ],
    ))
}

fn capability(row: &Rec, value: i64) -> Rec {
    replace(row, &[("page_server_read_availability", Val::Int(value))])
}

#[test]
fn one_native_decoder_retains_float_and_exact_bigint_statistics_for_both_paths() {
    let raw = native(&runtime_row(1_000));
    let TelemetryRecord::QueryRuntime(row) =
        decode::record(&raw, TelemetryStream::QueryRuntime, StatementReferences::default()).unwrap()
    else {
        panic!("runtime")
    };
    let selected = PlanSummary::decode(&single_fragment_summary()).unwrap();
    assert_eq!(row.page_server_reads.as_deref(), Some(selected.page_server_reads.statistics()));
    let statistics = row.page_server_reads.as_ref().unwrap();
    assert_eq!(statistics.last_pages.as_deref(), Some("9007199254740993"));
    assert_eq!(statistics.maximum_pages, Some(i64::MAX.to_string()));
    assert_eq!(statistics.average_pages, Some(12.5));
    assert_eq!(row.key.native.id, "1000");
    assert_eq!(row.key.native.qualifiers["execution_type"], "3");
    assert_eq!(row.key.native.qualifiers["replica_group_id"], "7");
    let PageServerReadSummary::NativeFragment { native: Some(native), .. } = selected.page_server_reads else {
        panic!("single original fragment")
    };
    assert_eq!(native.id, "17");
    assert_eq!(native.qualifiers["runtime_stats_interval_id"], "1");
    assert_eq!(native.qualifiers["execution_type"], "0");
}

#[test]
fn supported_null_zero_and_unavailable_profiles_never_collapse_to_one_numeric_state() {
    for (code, expected) in
        [(0, Availability::NotApplicable), (2, Availability::Unknown), (3, Availability::Unavailable)]
    {
        let row = capability(&runtime_row(1_000), code);
        let observed = decode(&row).unwrap();
        assert_eq!(observed.availability, expected);
        assert!(!observed.has_values());
        assert!(decode(&replace(&row, &[("last_page_server_io_reads", Val::Int(0))])).is_err());
    }
    let missing = decode(&capability(&runtime_row(1_000), 1)).unwrap();
    assert_eq!(missing.availability, Availability::Available);
    assert!(!missing.has_values());
    let zero =
        decode(&replace(&capability(&runtime_row(1_000), 1), &[("last_page_server_io_reads", Val::Int(0))])).unwrap();
    assert_eq!(zero.last_pages.as_deref(), Some("0"));
}

#[test]
fn native_integer_shapes_and_declared_columns_fail_closed_without_coercion() {
    for (name, value) in [
        ("last_page_server_io_reads", Val::Float(1.0)),
        ("min_page_server_io_reads", Val::Int(-1)),
        ("max_page_server_io_reads", Val::Text("9007199254740993".into())),
        ("avg_page_server_io_reads", Val::Float(f64::NAN)),
        ("stdev_page_server_io_reads", Val::Float(f64::INFINITY)),
    ] {
        assert!(decode(&replace(&native(&runtime_row(1_000)), &[(name, value)])).is_err(), "{name}");
    }
    let row = native(&runtime_row(1_000));
    let fields = row
        .columns()
        .filter(|(name, _)| *name != "last_page_server_io_reads")
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    assert!(decode(&Rec::of(&fields)).is_err(), "an absent projection column is not native NULL");
}

#[test]
fn mixed_fragments_keep_no_representative_last_or_unqualified_outcome_average() {
    let row = replace(
        &single_fragment_summary(),
        &[
            ("page_server_native_fragments", Val::Int(2)),
            ("page_server_native_row", Val::Null),
            ("page_server_native_interval", Val::Null),
            ("last_page_server_io_reads", Val::Null),
            ("stdev_page_server_io_reads", Val::Null),
        ],
    );
    let summary = PlanSummary::decode(&row).unwrap().page_server_reads;
    assert!(
        matches!(&summary, PageServerReadSummary::Derived { native_fragments, execution_type: Some(outcome), .. } if native_fragments=="2" && outcome=="0")
    );
    assert_eq!(summary.statistics().average_pages, Some(12.5));
    assert!(PlanSummary::decode(&replace(&row, &[("last_page_server_io_reads", Val::Int(1))])).is_err());
    assert!(
        PlanSummary::decode(&replace(&row, &[("page_server_native_outcome", Val::Null)])).is_err(),
        "an average cannot cross outcome cohorts"
    );
    let mixed = replace(
        &row,
        &[
            ("page_server_native_outcome", Val::Null),
            ("avg_page_server_io_reads", Val::Null),
            ("min_page_server_io_reads", Val::Null),
            ("max_page_server_io_reads", Val::Null),
        ],
    );
    assert!(PlanSummary::decode(&mixed).unwrap().page_server_reads.is_valid());
}

#[test]
fn native_templates_bound_capability_and_preserve_existing_membership_without_unqualified_column_references() {
    let metadata = availability!();
    assert!(metadata.contains("object_id=OBJECT_ID(N'sys.query_store_runtime_stats')"));
    assert!(metadata.contains("WHEN DATABASEPROPERTYEX(DB_NAME(),'Edition') IS NULL THEN 2"));
    assert!(metadata.contains("=5 THEN 1 ELSE 3 END ELSE 2 END"));
    for (name, _) in FIELDS {
        assert_eq!(metadata.matches(&format!("N'{name}'")).count(), 1);
    }
    for unavailable in [Availability::Unknown, Availability::NotApplicable, Availability::Unavailable] {
        let raw = crate::query_store::catalog_sql::runtime(false, unavailable);
        let summary = crate::query_store::runtime::summary_sql(
            false,
            false,
            unavailable,
            crate::query_store::replicas::Population::Unpartitioned,
        );
        for (name, _) in FIELDS {
            assert!(!raw.contains(&format!("r.{name}")));
            assert!(!summary.contains(&format!("rs.{name}")));
        }
    }
    let summary = summary_columns(Availability::Available);
    assert!(summary.contains("COUNT(DISTINCT rs.execution_type)=1"));
    assert!(summary.contains("COUNT_BIG(*)=1 THEN MAX(rs.last_page_server_io_reads)"));
    assert!(summary.contains("COUNT_BIG(*)=1 THEN MAX(rs.stdev_page_server_io_reads)"));
    let raw = crate::query_store::catalog_sql::runtime(true, Availability::Available);
    assert!(raw.contains("r.runtime_stats_interval_id=@P2") && raw.contains("r.replica_group_id=@P3"));
    assert!(raw.contains("ORDER BY r.plan_id,r.execution_type,r.replica_group_id,r.runtime_stats_id"));
    assert!(!summary.contains("total_page_server") && !summary.contains("avg_physical_io_reads+"));
}

#[tokio::test]
async fn changed_capability_before_or_during_native_rows_retires_without_fallback_work() {
    let layout = replace(&capability(&layout_row(), 1), &[("engine_edition", Val::Int(5))]);
    for at_window in [true, false] {
        let mut replies =
            vec![Ok(vec![layout.clone()]), Ok(vec![capability(&azure_row(), if at_window { 3 } else { 1 })])];
        if !at_window {
            // The native projection clears its statistics when the Hyperscale guard no longer holds.
            let changed = replace(&single_fragment_summary(), &FIELDS.map(|(name, _)| (name, Val::Null)));
            replies.push(Ok(vec![capability(&changed, 2)]));
        }
        let mut reader = Reader::new(FakeRead::new(replies));
        let Err(error) = reader.summary(60).await else { panic!("changed native profile was accepted") };
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        let before = reader.queries();
        assert!(reader.read_rows("must not execute").await.is_err());
        assert_eq!(reader.queries(), before);
        assert_eq!(before, if at_window { 2 } else { 3 });
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn existing_final_metadata_recheck_withdraws_changed_capability_without_new_queries() {
    let layout = replace(&capability(&layout_row(), 1), &[("engine_edition", Val::Int(5))]);
    let mut reader = Reader::new(FakeRead::new(vec![
        Ok(vec![layout]),
        Ok(vec![capability(&azure_row(), 1)]),
        Ok(vec![single_fragment_summary()]),
        Ok(vec![capability(&azure_row(), 3)]),
    ]));
    assert_eq!(reader.summary(60).await.unwrap().len(), 1);
    assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::RuntimePopulationChanged);
    assert_eq!(reader.queries(), 4);
    assert!(reader.read_rows("must not execute").await.is_err());
    assert_eq!(reader.queries(), 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
