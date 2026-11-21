use super::*;

fn native(object: &str, name: &str, instance: &str, code: i64, value: i64) -> Rec {
    replace(
        &base(),
        &[
            ("native_object", Val::Text(format!("SQLServer:{object}"))),
            ("native_name", Val::Text(name.into())),
            ("native_instance", Val::Text(instance.into())),
            ("native_type", Val::Int(code)),
            ("value", Val::Int(value)),
        ],
    )
}
fn pair(hits: i64, total: i64) -> Vec<Rec> {
    vec![
        native("Buffer Manager", "Buffer cache hit ratio", "", 537_003_264, hits),
        native("Buffer Manager", "Buffer cache hit ratio base", "", 1_073_939_712, total),
    ]
}
fn buffer_ratio(read: &PerformanceRead) -> &NativeCounterRatio {
    read.sample.ratios.iter().find(|ratio| ratio.kind == CounterRatioKind::BufferCacheHit).unwrap()
}
#[tokio::test]
async fn common_inputs_share_one_native_dispatch_with_exact_type_scope_and_real_fraction_base() {
    let mut rows = vec![
        base(),
        native("Databases", "Active Transactions", "_Total", 65_792, 0),
        native("Transactions", "Free Space in tempdb (KB)", "", 65_792, 9007199254740993),
        native("Buffer Manager", "Page reads/sec", "", 272_696_576, 9007199254740993),
        native("SQL Errors", "Errors/sec", "Info Errors", 272_696_576, 3),
    ];
    rows.extend(pair(3, 4));
    let mut reader = FakeRead::new(vec![Ok(rows)]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_COUNTERS + 1);
    assert_eq!(read.native_rows, 7);
    let profile = culprit_core::Engine::Mssql.spec().performance_counters().unwrap();
    assert!(profile.common_requested(&read.sample));
    let free = read.sample.counters.iter().find(|counter| counter.native_name == "Free Space in tempdb (KB)").unwrap();
    assert_eq!(free.value.integer(), Some(9007199254740993));
    assert_eq!((free.unit, free.nature), (CounterUnit::Kibibytes, CounterNature::Gauge));
    let pages = read.sample.counters.iter().find(|counter| counter.native_name == "Page reads/sec").unwrap();
    assert_eq!((pages.unit, pages.nature), (CounterUnit::Pages { bytes_per_page: 8192 }, CounterNature::Cumulative));
    assert!(
        read.sample
            .counters
            .iter()
            .all(|counter| counter.scope == MeasurementScope::Server && counter.reset == ResetMarker::Unknown)
    );
    assert_eq!(buffer_ratio(&read).percent(&read.sample).as_deref(), Some("75.00"));
    assert_eq!(
        application(&read.sample.counters).len(),
        1,
        "new informational counters never enter old admission projections"
    );
    let sql = &reader.calls[0].sql;
    for field in profile.common_fields {
        assert!(sql.contains(&format!("counter_name=N'{}'", field.native_name)), "{}", field.native_name);
    }
    assert!(sql.contains("instance_name) IN (N'_Total')"));
    assert!(sql.contains("N'Info Errors'"));
    assert!(!sql.contains("ORDER BY"));
    assert!(!sql.contains("sys.databases"));
}
#[tokio::test]
async fn absence_wrong_type_and_invalid_fraction_inputs_never_become_a_healthy_hit_percentage() {
    use culprit_core::telemetry::CounterRatioIssue as Issue;
    for (name, rows, issue) in [
        ("missing-both", vec![base()], Issue::MissingNumerator),
        ("missing-base", vec![pair(3, 4).remove(0)], Issue::MissingBase),
        ("zero-base", pair(0, 0), Issue::ZeroBase),
        ("outside-subset", pair(5, 4), Issue::OutsideSubset),
        ("negative", pair(-1, 4), Issue::InvalidNumerator),
        (
            "wrong-type",
            vec![native("Buffer Manager", "Buffer cache hit ratio", "", 65_792, 3), pair(3, 4).remove(1)],
            Issue::UnsupportedCounterType,
        ),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
        let ratio = buffer_ratio(&read);
        assert!(ratio.percent(&read.sample).is_none(), "{name}");
        assert!(ratio.issues(&read.sample).contains(&issue), "{name}");
        assert_eq!(reader.calls.len(), 1, "{name}");
    }
}
#[tokio::test]
async fn foreign_instance_object_duplicate_or_changed_native_owner_refuses_without_retry() {
    let original = native("Databases", "Transactions/sec", "_Total", 272_696_576, 1);
    for (name, rows, limit) in [
        (
            "foreign-db-label",
            vec![replace(&original, &[("native_instance", Val::Text("application-db".into()))])],
            MAX_COUNTERS,
        ),
        (
            "object-collision",
            vec![replace(&original, &[("native_object", Val::Text("SQLServer:Databases other".into()))])],
            MAX_COUNTERS,
        ),
        ("duplicate", vec![original.clone(), original.clone()], MAX_COUNTERS),
        ("owner", vec![replace(&original, &[("process_id", Val::Int(43))])], MAX_COUNTERS),
        ("clock", vec![replace(&original, &[("counter_observed_at", Val::Null)])], MAX_COUNTERS),
        ("lookahead", vec![original.clone(), base()], 1),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, limit, &expected()).await.is_err(), "{name}");
        assert_eq!(reader.calls.len(), 1, "{name}");
    }
}
#[tokio::test]
async fn default_selection_has_no_common_extension_and_suffix_does_not_authorize_meaning() {
    let mut reader = FakeRead::new(vec![Ok(vec![super::super::super::tests::row()])]);
    let baseline = super::super::super::read(&mut reader, MAX_COUNTERS).await.unwrap();
    let profile = culprit_core::Engine::Mssql.spec().performance_counters().unwrap();
    assert!(!profile.common_requested(&baseline.sample));
    for field in profile.common_fields {
        assert!(
            !reader.calls[0].sql.contains(&format!("counter_name=N'{}'", field.native_name)),
            "{}",
            field.native_name
        );
    }
    let mut reader = FakeRead::new(vec![Ok(vec![native("Buffer Manager", "Page reads/sec", "", 65_792, 7)])]);
    let read = read(&mut reader, MAX_COUNTERS, &expected()).await.unwrap();
    assert_eq!(
        (read.sample.counters[0].nature, read.sample.counters[0].unit),
        (CounterNature::Unknown, CounterUnit::Unknown)
    );
    assert_eq!(read.sample.counters[0].value.integer(), Some(7));
}
