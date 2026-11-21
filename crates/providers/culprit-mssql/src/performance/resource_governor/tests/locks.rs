use super::*;
fn native(name: &str, code: i64, value: i64) -> Rec {
    replace(
        &base(),
        &[
            ("native_object", Val::Text("SQLServer:Locks".into())),
            ("native_name", Val::Text(name.into())),
            ("native_instance", Val::Text("_Total".into())),
            ("native_type", Val::Int(code)),
            ("value", Val::Int(value)),
        ],
    )
}
fn pair(wait: i64, count: i64) -> Vec<Rec> {
    vec![native("Average Wait Time (ms)", 1073874176, wait), native("Average Wait Time Base", 1073939712, count)]
}
fn average(read: &PerformanceRead) -> &NativeCounterRatio {
    read.sample.ratios.iter().find(|ratio| ratio.kind == CounterRatioKind::LockWaitAverageInputs).unwrap()
}
#[tokio::test]
async fn total_lock_counts_and_average_inputs_share_the_existing_dispatch_without_a_mean_or_default_expansion() {
    let mut rows = pair(4500, 3);
    for name in ["Lock Requests/sec", "Lock Waits/sec", "Lock Timeouts/sec", "Lock Timeouts (timeout > 0)/sec"] {
        rows.push(native(name, 272696576, 9_007_199_254_740_993));
    }
    let mut reader = FakeRead::new(vec![Ok(rows)]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_COUNTERS + 1);
    assert_eq!(read.native_rows, 6);
    assert_eq!(read.sample.counters.len(), 6);
    assert!(application(&read.sample.counters).is_empty());
    assert_eq!(average(&read).issues(&read.sample), [CounterRatioIssue::IntervalRequired]);
    assert!(average(&read).percent(&read.sample).is_none());
    for field in culprit_core::Engine::Mssql
        .spec()
        .performance_counters()
        .unwrap()
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::Locks)
    {
        assert!(reader.calls[0].sql.contains(&format!("RIGHT(RTRIM(object_name),LEN(N':Locks'))=N':Locks' AND counter_name=N'{}' AND RTRIM(instance_name) IN (N'_Total')",field.native_name)));
        assert!(!super::super::super::sql().contains(&format!("counter_name=N'{}'", field.native_name)));
    }
    assert!(
        read.sample
            .counters
            .iter()
            .all(|row| row.scope == MeasurementScope::Server && row.reset == ResetMarker::Unknown)
    );
}
#[tokio::test]
async fn missing_negative_zero_or_wrong_native_average_base_is_retained_without_a_cooked_value() {
    use CounterRatioIssue::*;
    for (cause, rows, issue) in [
        ("missing", vec![pair(4500, 3).remove(0)], MissingBase),
        ("missing-both", vec![base()], MissingNumerator),
        ("negative", pair(4500, -1), InvalidBase),
        ("zero", pair(0, 0), ZeroBase),
        (
            "fraction-numerator",
            vec![native("Average Wait Time (ms)", 537003264, 3), pair(4500, 3).remove(1)],
            UnsupportedCounterType,
        ),
        (
            "wrong-base",
            vec![pair(4500, 3).remove(0), native("Average Wait Time Base", 65792, 3)],
            UnsupportedCounterType,
        ),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
        let average = average(&read);
        assert!(average.percent(&read.sample).is_none(), "{cause}");
        assert!(average.issues(&read.sample).contains(&issue), "{cause}");
        assert!(!average.issues(&read.sample).contains(&OutsideSubset), "{cause}");
        if cause == "missing-both" {
            assert!(average.numerator.is_none() && average.base.is_none());
        }
        assert_eq!(reader.calls.len(), 1, "{cause}");
    }
}
#[tokio::test]
async fn foreign_lock_resource_kind_or_changed_instance_and_tighter_caller_never_yield_a_partial_pair() {
    for (cause, rows, limit) in [
        (
            "instance",
            vec![replace(&pair(4500, 3).remove(1), &[("native_instance", Val::Text("Page".into()))])],
            MAX_COUNTERS,
        ),
        (
            "object",
            vec![replace(&pair(4500, 3).remove(1), &[("native_object", Val::Text("SQLServer:Other Locks".into()))])],
            MAX_COUNTERS,
        ),
        ("owner", vec![replace(&pair(4500, 3).remove(0), &[("process_id", Val::Int(43))])], MAX_COUNTERS),
        ("limit", pair(4500, 3), 1),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, limit, &expected()).await.is_err(), "{cause}");
        assert_eq!(reader.calls.len(), 1, "{cause}");
    }
}
