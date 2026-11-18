use super::*;
use crate::{
    query_store::window_sql::{self, FrozenWindow, WindowVerification, tests::azure_row},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn row() -> Rec {
    let original = azure_row();
    let mut fields = original.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("configured_log_database_id", Val::Int(7)),
        ("configured_log_group_id", Val::Int(12)),
        ("configured_log_workload_group", Val::Int(9_007_199_254_740_993)),
        ("configured_log_resource_pool", Val::Int(0)),
        ("configured_log_instance", Val::Null),
        ("configured_log_observed_at", Val::Text("2020-01-01T07:00:00Z".into())),
    ]);
    Rec::of(&fields)
}

#[tokio::test]
async fn requested_configuration_uses_one_existing_native_row_and_exact_integer_decoder() {
    let mut io = FakeRead::new(vec![Ok(vec![row()])]);
    let read = read_azure_detail(&mut io, Detail::ConfiguredLogRates).await.unwrap();
    let point = read.configured_log_rates.unwrap().unwrap();
    assert_eq!(point.workload_group_bytes_per_second.as_deref(), Some("9007199254740993"));
    assert_eq!(point.resource_pool_bytes_per_second.as_deref(), Some("0"));
    assert!(point.instance_bytes_per_second.is_none());
    assert_eq!(io.calls.len(), 1);
    assert_eq!(io.calls[0].sql.matches("sys.dm_user_db_resource_governance").count(), 1);
    assert!(io.calls[0].sql.contains("database_id=DB_ID() AND database_name=DB_NAME()"));
    assert_eq!(io.closed, 0, "the original caller owns settlement");
}

#[test]
fn identity_only_and_final_authority_templates_do_not_acquire_configuration() {
    assert_eq!(
        culprit_core::telemetry::digest(azure_query().as_bytes()),
        "8aa91f868414592ce53108b8effdd2dc352ad5cd68992e2fc812df150cd0cfb7"
    );
    assert_eq!(
        culprit_core::telemetry::digest(window_sql::metadata_sql(true, false, 5).as_bytes()),
        "745ac1a2983b1b6851da1e7fccbfc978b79a12b3f4be80e8a97b37433e3918fb"
    );
    assert_eq!(
        culprit_core::telemetry::digest(window_sql::metadata_sql(true, true, 5).as_bytes()),
        "7afc6f6af049f0264ede7654c26d425c67643e17ba73c2693cc6e572a2de265a"
    );
    assert_eq!(crate::query_store::runtime::layout_query(Detail::Identity), crate::query_store::runtime::LAYOUT);
    for edition in [3, 5, 8] {
        for final_read in [false, true] {
            let ordinary = window_sql::metadata_sql(true, final_read, edition);
            assert!(!ordinary.contains("primary_max_log_rate"));
            assert!(!ordinary.contains("pool_max_log_rate"));
            assert!(!ordinary.contains("instance_max_log_rate"));
            let requested = window_sql::metadata_sql_detail(true, final_read, edition, Detail::ConfiguredLogRates);
            assert_eq!(requested.contains("primary_max_log_rate"), edition == 5);
            assert_eq!(requested.matches("sys.dm_user_db_resource_governance").count(), usize::from(edition == 5));
        }
    }
    assert!(!azure_query().contains("max_log_rate"));
}

#[test]
fn later_limit_changes_do_not_rewrite_original_configuration_or_invalidate_retained_history() {
    let original = row();
    let mut window = FrozenWindow::decode_detail(&original, 60, true, Detail::ConfiguredLogRates).unwrap();
    let changed = replace(
        &original,
        &[("configured_log_workload_group", Val::Int(1)), ("window_clock", Val::Text("2020-01-01T07:00:01Z".into()))],
    );
    assert_eq!(window.verify(&changed).unwrap(), WindowVerification::Unchanged);
    assert_eq!(
        window.configured_log_rates.unwrap().unwrap().workload_group_bytes_per_second.as_deref(),
        Some("9007199254740993")
    );
}

#[test]
fn malformed_optional_configuration_is_a_separate_gap_and_never_repairs_native_identity() {
    for field in [
        "configured_log_database_id",
        "configured_log_group_id",
        "configured_log_workload_group",
        "configured_log_resource_pool",
        "configured_log_instance",
    ] {
        let malformed = replace(&row(), &[(field, Val::Int(-1))]);
        let window = FrozenWindow::decode_detail(&malformed, 60, true, Detail::ConfiguredLogRates).unwrap();
        assert!(window.configured_log_rates.is_err());
        assert!(window.retained, "an independent malformed limit does not erase query history");
    }
    let unknown = replace(
        &row(),
        &[("native_identity_rows", Val::Int(0)), ("incarnation", Val::Null), ("physical_incarnation", Val::Null)],
    );
    let window = FrozenWindow::decode_detail(&unknown, 60, true, Detail::ConfiguredLogRates).unwrap();
    assert!(window.configured_log_rates.unwrap().is_none());
}
