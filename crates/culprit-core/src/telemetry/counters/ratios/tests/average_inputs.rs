use super::*;
fn point() -> NativeCounterSet {
    let mut point = sample();
    point.ratios[0].kind = CounterRatioKind::LockWaitAverageInputs;
    let numerator = &mut point.counters[0];
    numerator.nature = CounterNature::AverageNumerator;
    numerator.unit = CounterUnit::Milliseconds;
    numerator.native_type = Some("1073874176".into());
    numerator.value = MeasurementValue::Integer("4500".into());
    numerator.native_object = "SQLServer:Locks".into();
    numerator.native_instance = Some("_Total".into());
    numerator.native_name = "Average Wait Time (ms)".into();
    let base = &mut point.counters[1];
    base.native_object = "SQLServer:Locks".into();
    base.native_instance = Some("_Total".into());
    base.native_name = "Average Wait Time Base".into();
    base.value = MeasurementValue::Integer("3".into());
    point
}
#[test]
fn an_average_numerator_is_not_a_subset_and_a_complete_original_point_never_becomes_a_mean_or_percent() {
    let mut point = point();
    assert!(point.is_valid());
    assert_eq!(CounterNature::from_windows_type(1_073_874_176), CounterNature::AverageNumerator);
    assert_eq!(point.ratios[0].issues(&point), [CounterRatioIssue::IntervalRequired]);
    assert!(point.ratios[0].percent(&point).is_none());
    assert_eq!(point.counters[0].value.integer(), Some(4500));
    assert_eq!(point.counters[1].value.integer(), Some(3));
    point.counters[0].value = MeasurementValue::Integer("0".into());
    assert_eq!(point.ratios[0].issues(&point), [CounterRatioIssue::IntervalRequired]);
    point.counters[1].value = MeasurementValue::Integer("0".into());
    assert!(point.ratios[0].issues(&point).contains(&CounterRatioIssue::ZeroBase));
    assert!(point.ratios[0].percent(&point).is_none());
}
#[test]
fn average_inputs_require_the_same_original_type_object_instance_scope_reset_and_read_clock() {
    use CounterRatioIssue::*;
    for (cause, issue) in [
        ("missing", MissingBase),
        ("object", IncompatibleInputs),
        ("instance", IncompatibleInputs),
        ("database", IncompatibleInputs),
        ("pool", IncompatibleInputs),
        ("reset", IncompatibleInputs),
        ("type", UnsupportedCounterType),
        ("clock", ReadClockUnavailable),
        ("unknown", UnknownProfile),
    ] {
        let mut point = point();
        match cause {
            "missing" => {
                point.counters.pop();
                point.ratios[0].base = None;
            }
            "object" => point.counters[1].native_object = "foreign:Locks".into(),
            "instance" => point.counters[1].native_instance = Some("Page".into()),
            "database" | "pool" => {
                point.scope =
                    if cause == "database" { crate::MeasurementScope::Database } else { crate::MeasurementScope::Pool };
                point.counters.iter_mut().for_each(|row| row.scope = point.scope);
            }
            "reset" => point.counters[1].reset = crate::ResetMarker::Generation("other".into()),
            "type" => {
                let row = &mut point.counters[0];
                row.native_type = Some("42".into());
                row.nature = CounterNature::Unknown;
                row.unit = CounterUnit::Unknown;
            }
            "clock" => point.ratios[0].observed_at = None,
            "unknown" => point.ratios[0].support = CounterRatioSupport::Unknown,
            _ => unreachable!(),
        }
        assert!(point.is_valid(), "{cause}");
        assert!(point.ratios[0].issues(&point).contains(&issue), "{cause}");
        assert!(point.ratios[0].issues(&point).contains(&IntervalRequired), "{cause}");
        assert!(point.ratios[0].percent(&point).is_none(), "{cause}");
    }
}
#[test]
fn negative_original_average_inputs_stay_rejected_native_evidence_and_never_zero() {
    let mut point = point();
    let base = point.counters.pop().unwrap();
    point.ratio_gaps.push(CounterRatioGap {
        key: base.key,
        raw_value: "-1".into(),
        native_object: base.native_object,
        native_name: base.native_name,
        native_instance: base.native_instance,
        native_type: base.native_type.unwrap(),
    });
    assert!(point.is_valid());
    assert!(point.ratios[0].issues(&point).contains(&CounterRatioIssue::InvalidBase));
    assert!(point.ratios[0].percent(&point).is_none());
}
