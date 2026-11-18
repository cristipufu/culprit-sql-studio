use super::*;
use culprit_core::telemetry::*;

fn native(object: &str, name: &str, instance: &str, code: i64, value: i64) -> Rec {
    replace(
        &row(),
        &[
            ("native_object", Val::Text(object.into())),
            ("native_name", Val::Text(name.into())),
            ("native_instance", Val::Text(instance.into())),
            ("native_type", Val::Int(code)),
            ("value", Val::Int(value)),
        ],
    )
}

#[tokio::test]
async fn all_fixed_native_fields_use_one_query_without_entering_the_original_application_projection() {
    let mut rows = Vec::new();
    for field in specs() {
        let instances = match field.instances {
            PerformanceCounterInstances::Named(instances) => instances.to_vec(),
            _ => vec![""],
        };
        for instance in instances {
            let code = match field.nature {
                CounterNature::Cumulative => 272_696_576,
                CounterNature::Gauge => 65_792,
                CounterNature::FractionNumerator => 537_003_264,
                CounterNature::FractionBase => 1_073_939_712,
                CounterNature::Unknown => unreachable!(),
            };
            rows.push(native(
                &format!("SQLServer:{}", field.object_family.unwrap_or("Buffer Manager")),
                field.native_name,
                instance,
                code,
                9_007_199_254_740_993,
            ));
        }
    }
    // The same complete profile exceeds the obsolete32-row native parity caller. That tighter caller remains
    // a refusal: no successful prefix, extra read or fallback is allowed. This does not model Attention ack.
    let mut tight = FakeRead::new(vec![Ok(rows.clone())]);
    let error = read(&mut tight, 32).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(tight.calls.len(), 1);
    assert_eq!(tight.calls[0].limit, 33);
    let mut reader = FakeRead::new(vec![Ok(rows)]);
    let read = read(&mut reader, MAX_COUNTERS).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_COUNTERS + 1);
    assert_eq!(reader.calls[0].sql, sql());
    assert_eq!(read.native_rows, 59);
    assert_eq!(application(&read.sample.counters).len(), 6);
    assert!(read.sample.counters.iter().all(|counter| counter.value.integer() == Some(9_007_199_254_740_993)));
    assert_eq!(read.sample.ratios.iter().filter(|ratio| ratio.kind == CounterRatioKind::PlanCacheHit).count(), 6);
}

#[tokio::test]
async fn fixed_instances_refuse_foreign_databases_and_unbounded_cache_labels_without_a_second_read() {
    for input in [
        native("SQLServer:Databases", "Log Bytes Flushed/sec", "app", 272_696_576, 5),
        native("SQLServer:Plan Cache", "Cache Pages", "unrecognized-cache", 65_792, 5),
        native("SQLServer:Memory Manager", "Memory Grants Pending", "app", 65_792, 0),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![input])]);
        assert!(read(&mut reader, MAX_COUNTERS).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let statement = sql();
    assert!(statement.contains("N'Temporary Tables & Table Variables'"));
    assert!(statement.contains("counter_name=N'Log Bytes Flushed/sec' AND RTRIM(instance_name) IN (N'_Total')"));
    assert!(!statement.contains("DB_NAME() AS native_instance"));
}

#[tokio::test]
async fn missing_zero_unknown_type_and_row_overflow_are_not_interchangeable() {
    let rows = vec![
        native("SQLServer:Memory Manager", "Memory Grants Pending", "", 65_792, 0),
        native("SQLServer:Memory Manager", "Granted Workspace Memory (KB)", "", 42, 9_007_199_254_740_993),
    ];
    let observed = read(&mut FakeRead::new(vec![Ok(rows)]), MAX_COUNTERS).await.unwrap();
    assert_eq!(observed.sample.counters[0].value.integer(), Some(0));
    assert_eq!(observed.sample.counters[1].unit, CounterUnit::Unknown);
    assert!(!observed.sample.counters.iter().any(|row| row.native_name == "Maximum Workspace Memory (KB)"));
    assert!(application(&observed.sample.counters).is_empty());
    let error = read(&mut FakeRead::new(vec![Ok(vec![row(); MAX_COUNTERS + 1])]), MAX_COUNTERS).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
}

#[tokio::test]
async fn cache_pairs_never_cross_native_instances_and_metadata_changes_refuse_the_entire_read() {
    let numerator = native("SQLServer:Plan Cache", "Cache Hit Ratio", "SQL Plans", 537_003_264, 1);
    let base = native("SQLServer:Plan Cache", "Cache Hit Ratio Base", "Object Plans", 1_073_939_712, 2);
    let observed =
        read(&mut FakeRead::new(vec![Ok(vec![numerator.clone(), base.clone()])]), MAX_COUNTERS).await.unwrap();
    assert_eq!(observed.sample.ratios.len(), 2);
    assert!(observed.sample.ratios.iter().all(|ratio| ratio.percent(&observed.sample).is_none()));
    let changed = replace(&base, &[("engine_edition", Val::Int(5))]);
    let error = read(&mut FakeRead::new(vec![Ok(vec![numerator, changed])]), MAX_COUNTERS).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
}
