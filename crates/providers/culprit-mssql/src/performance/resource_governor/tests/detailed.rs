use super::*;
fn native(name: &str, code: i64, value: i64) -> Rec {
    replace(
        &base(),
        &[
            ("native_object", Val::Text("SQLServer:Access Methods".into())),
            ("native_name", Val::Text(name.into())),
            ("native_instance", Val::Text(String::new())),
            ("native_type", Val::Int(code)),
            ("value", Val::Int(value)),
        ],
    )
}
fn rows() -> Vec<Rec> {
    culprit_core::Engine::Mssql
        .spec()
        .performance_counters()
        .unwrap()
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::AccessMethods)
        .map(|field| native(field.native_name, 272696576, 9_007_199_254_740_993))
        .collect()
}
#[tokio::test]
async fn six_access_inputs_share_one_existing_dispatch_and_exact_suffix_without_expanding_the_ordinary_read() {
    let mut reader = FakeRead::new(vec![Ok(rows())]);
    let observed = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_COUNTERS + 1);
    assert_eq!(observed.native_rows, 6);
    assert!(application(&observed.sample.counters).is_empty());
    assert!(observed.resource_governor.is_empty());
    for row in &observed.sample.counters {
        assert_eq!((row.unit, row.nature), (CounterUnit::Count, CounterNature::Cumulative));
        assert_eq!(row.scope, MeasurementScope::Server);
        assert_eq!(row.reset, ResetMarker::Unknown);
        assert_eq!(row.value.integer(), Some(9_007_199_254_740_993));
        assert_eq!(row.native_instance, None);
        assert!(reader.calls[0].sql.contains(&format!("RIGHT(RTRIM(object_name),LEN(N':Access Methods'))=N':Access Methods' AND counter_name=N'{}' AND RTRIM(instance_name)=N''",row.native_name)));
        assert!(!super::super::super::sql().contains(&format!("counter_name=N'{}'", row.native_name)));
    }
    assert!(!reader.calls[0].sql.contains("LIKE N'%Access Methods%'"));
    assert!(!reader.calls[0].sql.contains("ORDER BY"));
    let mut ordinary = FakeRead::new(vec![Ok(rows())]);
    assert!(super::super::super::read(&mut ordinary, MAX_COUNTERS).await.is_err());
    assert_eq!(ordinary.calls.len(), 1);
}
#[tokio::test]
async fn access_native_zero_missing_unknown_type_and_caller_overflow_never_become_repaired_or_renewed_rows() {
    let mut reader =
        FakeRead::new(vec![Ok(vec![native("Full Scans/sec", 272696576, 0), native("Page Splits/sec", 65792, 7)])]);
    let observed = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(observed.sample.counters.len(), 2);
    assert_eq!(observed.sample.counters[0].value.integer(), Some(0));
    assert_eq!(
        (observed.sample.counters[1].nature, observed.sample.counters[1].unit),
        (CounterNature::Unknown, CounterUnit::Unknown)
    );
    assert!(!observed.sample.counters.iter().any(|row| row.native_name == "Workfiles Created/sec"));
    let mut tight = FakeRead::new(vec![Ok(rows())]);
    let error = read(&mut tight, 5, &expected()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(tight.calls.len(), 1);
    assert_eq!(tight.calls[0].limit, 6);
}
#[tokio::test]
async fn access_foreign_object_instance_duplicate_owner_or_clock_refuses_the_same_completed_read() {
    let original = native("Forwarded Records/sec", 272696576, 3);
    for (case, changed) in [
        ("object", vec![replace(&original, &[("native_object", Val::Text("SQLServer:Other Access Methods".into()))])]),
        ("suffix", vec![replace(&original, &[("native_object", Val::Text("SQLServer:Access Methods other".into()))])]),
        ("database", vec![replace(&original, &[("native_instance", Val::Text("app".into()))])]),
        ("duplicate", vec![original.clone(), original.clone()]),
        ("owner", vec![replace(&original, &[("process_id", Val::Int(43))])]),
        ("clock", vec![replace(&original, &[("counter_observed_at", Val::Null)])]),
        ("null", vec![replace(&original, &[("value", Val::Null)])]),
        ("negative", vec![native("Forwarded Records/sec", 272696576, -1)]),
    ] {
        let mut reader = FakeRead::new(vec![Ok(changed)]);
        assert!(read(&mut reader, MAX_COUNTERS, &expected()).await.is_err(), "{case}");
        assert_eq!(reader.calls.len(), 1, "{case}");
    }
}

#[tokio::test]
async fn access_permission_or_cancellation_failure_is_preserved_without_followup_or_replacement() {
    let denied = culprit_core::Error::new(
        culprit_core::ErrorCode::PermissionDenied,
        culprit_core::t!("view.performance_counters.summary.unavailable"),
    );
    let mut reader = FakeRead::new(vec![Err(ReadError::Source(denied.clone()))]);
    let actual = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap_err();
    assert_eq!(actual.code, denied.code);
    assert_eq!(actual.detail, denied.detail);
    assert_eq!(reader.calls.len(), 1);
    for stop in [CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed, CollectionStop::QueryLimit] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let actual = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&actual), stop);
        assert_eq!(reader.calls.len(), 1);
    }
}
