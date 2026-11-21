use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn expected() -> instance::Context {
    instance::observation(&instance::test_support::metadata())
        .unwrap()
        .context_from_epoch(&instance::test_support::epoch(100))
        .unwrap()
        .unwrap()
}
fn base() -> Rec {
    let mut values = super::super::tests::row()
        .columns()
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    for row in [instance::test_support::metadata(), instance::test_support::epoch(100)] {
        values.extend(row.columns().map(|(key, value)| (key.to_owned(), value.clone())));
    }
    values.insert("observed_at".into(), Val::Text("2020-01-01T12:00:00.5Z".into()));
    for key in ["rg_group_id", "rg_pool_id", "rg_name", "rg_statistics_started_without_offset"] {
        values.insert(key.into(), Val::Null);
    }
    Rec::of(&values.iter().map(|(key, value)| (key.as_str(), value.clone())).collect::<Vec<_>>())
}
fn workload(name: &str, code: i64, value: i64) -> Rec {
    replace(
        &base(),
        &[
            ("native_object", Val::Text("SQLServer:Workload Group Stats".into())),
            ("native_name", Val::Text(name.into())),
            ("native_type", Val::Int(code)),
            ("value", Val::Int(value)),
            ("native_instance", Val::Text("app-database-name".into())),
            ("rg_name", Val::Text("app-database-name".into())),
            ("rg_group_id", Val::Int(3)),
            ("rg_pool_id", Val::Int(2)),
            ("rg_statistics_started_without_offset", Val::Text("2019-12-31T23:00:00.000".into())),
        ],
    )
}
fn pool() -> Rec {
    replace(
        &workload("Active memory grants count", 65_792, 0),
        &[
            ("native_object", Val::Text("SQLServer:Resource Pool Stats".into())),
            ("native_instance", Val::Text("native-pool".into())),
            ("rg_name", Val::Text("native-pool".into())),
            ("rg_group_id", Val::Null),
        ],
    )
}

#[tokio::test]
async fn one_native_query_retains_current_group_pool_ids_and_counts_without_db_ownership_or_rate() {
    let mut reader = FakeRead::new(vec![Ok(vec![
        base(),
        workload("Active requests", 65_792, 0),
        workload("Queued requests", 65_792, 2),
        workload("Blocked requests", 65_792, 1),
        workload("Requests completed/sec", 272_696_576, 9_007_199_254_740_993),
        pool(),
    ])]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_COUNTERS + 1);
    assert_eq!(read.sample.counters.len(), 1);
    assert_eq!(read.native_rows, 6);
    assert_eq!(read.resource_governor.len(), 2);
    assert!(read.resource_governor_complete);
    let group = read.resource_governor.iter().find(|sample| sample.counters.len() == 4).unwrap();
    let context = MssqlResourceGovernorCounterSet::from_sample(group).unwrap().context();
    assert_eq!((context.native_group_id.as_deref(), context.native_pool_id.as_deref()), (Some("3"), Some("2")));
    assert_eq!(context.statistics_started_without_offset.as_deref(), Some("2019-12-31T23:00:00.0000000"));
    let completed = group.counters.iter().find(|counter| counter.native_name == "Requests completed/sec").unwrap();
    assert_eq!((completed.nature, completed.unit), (CounterNature::Cumulative, CounterUnit::Count));
    assert_eq!(completed.value.integer(), Some(9_007_199_254_740_993));
    assert!(
        group
            .counters
            .iter()
            .all(|counter| counter.scope == MeasurementScope::Server && counter.reset == ResetMarker::Unknown)
    );
    assert!(!reader.calls[0].sql.contains("DB_NAME(w."));
    assert!(reader.calls[0].sql.contains("CONVERT(varbinary(256),w.name)"));
}

#[tokio::test]
async fn named_counter_without_native_catalog_ids_stays_exact_and_partial_without_guessing_a_database() {
    let unmapped = replace(
        &workload("Queued requests", 65_792, 7),
        &[
            ("rg_group_id", Val::Null),
            ("rg_pool_id", Val::Null),
            ("rg_name", Val::Null),
            ("rg_statistics_started_without_offset", Val::Null),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![base(), unmapped])]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert!(!read.resource_governor_complete);
    let point = MssqlResourceGovernorCounterSet::from_sample(&read.resource_governor[0]).unwrap();
    assert_eq!(point.context().native_group_id, None);
    assert_eq!(point.context().native_pool_id, None);
    assert_eq!(read.resource_governor[0].counters[0].value.integer(), Some(7));
}

#[tokio::test]
async fn unknown_native_type_is_retained_without_fabricating_gauge_or_cumulative_meaning() {
    let mut reader = FakeRead::new(vec![Ok(vec![base(), workload("Active requests", 12345, 9)])]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    let value = &read.resource_governor[0].counters[0];
    assert_eq!((value.nature, value.unit), (CounterNature::Unknown, CounterUnit::Unknown));
    assert_eq!(value.native_type.as_deref(), Some("12345"));
    assert_eq!(value.value.integer(), Some(9));
}

#[tokio::test]
async fn ordinary_query_does_not_select_governor_rows_or_metadata_and_uses_the_same_type_decoder() {
    let mut reader = FakeRead::new(vec![Ok(vec![super::super::tests::row()])]);
    let read = super::super::read(&mut reader, MAX_COUNTERS).await.unwrap();
    assert!(read.resource_governor.is_empty());
    for forbidden in ["Workload Group Stats", "Resource Pool Stats", "dm_resource_governor", "sys.dm_os_sys_info"] {
        assert!(!reader.calls[0].sql.contains(forbidden), "{forbidden}");
    }
    assert_eq!(read.sample.counters[0].nature, CounterNature::Cumulative);
}

#[tokio::test]
async fn absent_requested_population_is_partial_not_a_healthy_zero_or_an_invented_group() {
    let mut reader = FakeRead::new(vec![Ok(vec![base()])]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert!(read.resource_governor.is_empty());
    assert!(!read.resource_governor_complete);
    assert_eq!(resource_governor_requested_rows(&read.sample).unwrap(), Some(0));
}

#[tokio::test]
async fn owner_clock_masked_id_native_name_duplicate_and_row_limit_refuse_without_another_query() {
    let original = workload("Active requests", 65_792, 1);
    for (name, rows, limit) in [
        ("owner", vec![base(), replace(&original, &[("process_id", Val::Int(43))])], MAX_COUNTERS),
        (
            "clock",
            vec![base(), replace(&original, &[("counter_observed_at", Val::Text("2020-01-01T12:00:00.6Z".into()))])],
            MAX_COUNTERS,
        ),
        ("masked", vec![base(), replace(&original, &[("rg_group_id", Val::Int(0))])], MAX_COUNTERS),
        ("name", vec![base(), replace(&original, &[("rg_name", Val::Text("other".into()))])], MAX_COUNTERS),
        ("duplicate", vec![base(), original.clone(), original.clone()], MAX_COUNTERS),
        ("rows", vec![base(), original], 1),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, limit, &expected()).await.is_err(), "{name}");
        assert_eq!(reader.calls.len(), 1, "{name}");
    }
}
