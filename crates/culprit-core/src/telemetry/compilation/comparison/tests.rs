use super::*;

fn point() -> CompilationCounterPoint {
    let native = super::super::tests::observation();
    let row = OperationalObservation {
        source: "original-compiler-read".into(),
        key: native.key().unwrap(),
        fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
    };
    CompilationCounterPoint::from_observation(&row).unwrap()
}

#[test]
fn compact_baselines_preserve_native_average_and_wide_count_without_sql_or_memory() {
    let point = point();
    assert!(point.is_valid());
    assert_eq!(point.compilations, "9007199254740993");
    assert_eq!(point.average_duration_us, 1500.5);
    let json = serde_json::to_value(&point).unwrap();
    assert_eq!(json["average_duration_us"], 1500.5);
    assert!(json.get("statement").is_none() && json.get("memory").is_none());
    assert_eq!(serde_json::from_value::<CompilationCounterPoint>(json).unwrap(), point);
}

#[test]
fn native_mean_is_required_and_invalid_means_never_turn_into_a_zero_baseline() {
    for average in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let mut point = point();
        point.average_duration_us = average;
        assert!(!point.is_valid());
    }
    let mut json = serde_json::to_value(point()).unwrap();
    json.as_object_mut().unwrap().remove("average_duration_us");
    assert!(serde_json::from_value::<CompilationCounterPoint>(json).is_err());
    let mut point = point();
    point.average_duration_us = 0.0;
    assert!(point.is_valid(), "native zero remains distinct from unavailable or invalid metadata");
}
