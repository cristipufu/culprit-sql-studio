use super::*;
use culprit_core::telemetry::{CounterRatioIssue, CounterRatioSupport};

fn hs(row: &Rec) -> Rec {
    replace(row, &[("engine_edition", Val::Int(5)), ("database_edition", Val::Text("Hyperscale".into()))])
}
fn input(name: &str, value: i64, native_type: i64) -> Rec {
    hs(&replace(
        &row(),
        &[
            // A returned object is retained, not selected or interpreted by an invented RBPEX family string.
            ("native_object", Val::Text("actual-native-object".into())),
            ("native_name", Val::Text(name.into())),
            ("value", Val::Int(value)),
            ("native_type", Val::Int(native_type)),
        ],
    ))
}
fn pair(numerator: i64, base: i64) -> Vec<Rec> {
    vec![
        input("RBPEX cache hit ratio", numerator, 537_003_264),
        input("RBPEX cache hit ratio base", base, 1_073_939_712),
    ]
}

#[tokio::test]
async fn exact_native_fraction_pair_is_collected_once_with_shared_unknown_lifetime_and_original_clock() {
    let mut rows = vec![hs(&row())];
    rows.extend(pair(9_007_199_254_740_993, 9_007_199_254_740_994));
    let mut reader = FakeRead::new(vec![Ok(rows)]);
    let result = read(&mut reader, MAX_COUNTERS).await.unwrap();
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(result.native_rows, 3);
    let set = &result.sample;
    assert_eq!(set.counters[1].value.integer(), Some(9_007_199_254_740_993));
    assert_eq!(set.ratios[0].percent(set).as_deref(), Some("99.99"));
    assert_eq!(set.ratios[0].support, CounterRatioSupport::Supported);
    assert_eq!(set.ratios[0].observed_at.as_deref(), Some("2020-01-01T12:00:00.5Z"));
    assert!(set.counters.iter().all(|row| row.scope == MeasurementScope::Server && row.reset == ResetMarker::Unknown));
    assert_eq!(application(&set.counters).len(), 1);
    assert_eq!(sampled_rates(&set.counters, &set.counters, 1.0).len(), 1, "fraction inputs are never sampled rates");
}

#[tokio::test]
async fn native_object_instance_or_type_never_borrows_a_different_base() {
    for mutation in ["object", "instance", "type"] {
        let mut rows = pair(1, 2);
        let changes = match mutation {
            "object" => vec![("native_object", Val::Text("other-object".into()))],
            "instance" => vec![("native_instance", Val::Text("other-instance".into()))],
            "type" => vec![("native_type", Val::Int(65_792))],
            _ => panic!(),
        };
        rows[1] = replace(&rows[1], &changes);
        let result = read(&mut FakeRead::new(vec![Ok(rows)]), 32).await.unwrap();
        assert!(result.sample.ratios.iter().all(|ratio| ratio.percent(&result.sample).is_none()), "{mutation}");
        assert_eq!(result.sample.counters.len(), 2);
    }
    let mut duplicate = pair(1, 2);
    duplicate.push(duplicate[0].clone());
    assert!(read(&mut FakeRead::new(vec![Ok(duplicate)]), 32).await.is_err());
}

#[tokio::test]
async fn optional_invalid_missing_zero_and_impossible_inputs_preserve_independent_counters_and_native_gaps() {
    for (numerator, base, issue) in [
        (-1, 2, CounterRatioIssue::InvalidNumerator),
        (1, -2, CounterRatioIssue::InvalidBase),
        (0, 0, CounterRatioIssue::ZeroBase),
        (3, 2, CounterRatioIssue::OutsideSubset),
    ] {
        let mut rows = vec![hs(&row())];
        rows.extend(pair(numerator, base));
        let result = read(&mut FakeRead::new(vec![Ok(rows)]), 32).await.unwrap();
        assert_eq!(application(&result.sample.counters).len(), 1);
        let ratio = &result.sample.ratios[0];
        assert!(ratio.issues(&result.sample).contains(&issue));
        assert!(ratio.percent(&result.sample).is_none());
        if numerator < 0 || base < 0 {
            assert_eq!(result.sample.ratio_gaps.len(), 1);
        }
    }
    let result = read(&mut FakeRead::new(vec![Ok(vec![hs(&row())])]), 32).await.unwrap();
    let declaration = &result.sample.ratios[0];
    assert_eq!((&declaration.numerator, &declaration.base), (&None, &None));
    assert_eq!(result.sample.counters.len(), 1, "missing RBPEX rows never become synthetic native counter rows");
    assert!(declaration.percent(&result.sample).is_none());
    let zero = read(&mut FakeRead::new(vec![Ok(pair(0, 2))]), 32).await.unwrap();
    assert_eq!(zero.sample.ratios[0].percent(&zero.sample).as_deref(), Some("0.00"));
}

#[tokio::test]
async fn native_profile_and_same_read_clock_are_required_without_a_probe_or_role_shortcut() {
    for profile in [CounterRatioSupport::Unsupported, CounterRatioSupport::Unknown] {
        let rows = pair(1, 2)
            .into_iter()
            .map(|row| {
                replace(
                    &row,
                    &[
                        (
                            "engine_edition",
                            if profile == CounterRatioSupport::Unsupported { Val::Int(3) } else { Val::Null },
                        ),
                        ("database_edition", Val::Null),
                    ],
                )
            })
            .collect();
        let result = read(&mut FakeRead::new(vec![Ok(rows)]), 32).await.unwrap();
        assert_eq!(result.sample.ratios[0].support, profile);
        assert!(result.sample.ratios[0].percent(&result.sample).is_none());
    }
    let mut rows = pair(1, 2);
    rows[1] = replace(&rows[1], &[("counter_observed_at", Val::Text("2020-01-01T12:00:01Z".into()))]);
    let error = read(&mut FakeRead::new(vec![Ok(rows)]), 32).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    let boxed = read(&mut FakeRead::new(vec![Ok(vec![row()])]), 32).await.unwrap();
    assert!(boxed.sample.ratios.is_empty(), "ordinary boxed SQL is no positive RBPEX support proof");
}

#[test]
fn exact_fixed_selector_adds_no_object_family_guess_limit_or_extra_query() {
    let sql = sql();
    assert_eq!(sql.matches("FROM sys.dm_os_performance_counters").count(), 1);
    assert!(sql.contains("counter_name=N'RBPEX cache hit ratio'"));
    assert!(sql.contains("counter_name=N'RBPEX cache hit ratio base'"));
    assert!(!sql.contains("%RBPEX%"));
    assert!(sql.contains("DECLARE @counter_observed_at"));
    assert_eq!(MAX_COUNTERS, MAX_NATIVE_COUNTERS);
}

#[tokio::test]
async fn an_unavailable_native_clock_withholds_only_the_ratio_and_does_not_erase_known_profile_or_ordinary_measurements()
 {
    let rows = std::iter::once(hs(&row()))
        .chain(pair(1, 2))
        .map(|row| replace(&row, &[("counter_observed_at", Val::Null)]))
        .collect();
    let result = read(&mut FakeRead::new(vec![Ok(rows)]), 32).await.unwrap();
    assert_eq!(application(&result.sample.counters).len(), 1);
    let ratio = &result.sample.ratios[0];
    assert_eq!(ratio.support, CounterRatioSupport::Supported);
    assert_eq!(ratio.issues(&result.sample), vec![CounterRatioIssue::ReadClockUnavailable]);
    assert!(ratio.percent(&result.sample).is_none());
}
