use super::*;

fn sample() -> NativeCounterSet {
    let mut numerator = NativeCounterSample::gauge(
        "hits",
        9_007_199_254_740_993,
        CounterUnit::Count,
        crate::MeasurementScope::Server,
        "native-cache-object",
        "native-hits",
    );
    numerator.nature = CounterNature::FractionNumerator;
    numerator.native_type = Some("537003264".into());
    let mut base = numerator.clone();
    base.key = "base".into();
    base.native_name = "native-base".into();
    base.nature = CounterNature::FractionBase;
    base.native_type = Some("1073939712".into());
    base.value = MeasurementValue::Integer("9007199254740994".into());
    NativeCounterSet {
        identity: NativeIdentity::new("native-counter-read"),
        scope: crate::MeasurementScope::Server,
        counters: vec![numerator, base],
        ratio_gaps: vec![],
        properties: Default::default(),
        ratios: vec![NativeCounterRatio {
            kind: CounterRatioKind::LocalSsdCacheHit,
            numerator: Some("hits".into()),
            base: Some("base".into()),
            support: CounterRatioSupport::Supported,
            observed_at: Some("2020-01-01T12:00:00.1234567Z".into()),
        }],
    }
}

#[test]
fn ratio_keeps_exact_inputs_and_calculates_only_a_checked_aggregate_fraction() {
    let mut point = sample();
    assert!(point.is_valid());
    assert_eq!(point.ratios[0].percent(&point).as_deref(), Some("99.99"));
    assert_eq!(point.counters[0].value.integer(), Some(9_007_199_254_740_993));
    point.counters[0].value = MeasurementValue::Integer("0".into());
    assert_eq!(point.ratios[0].percent(&point).as_deref(), Some("0.00"));
    point.counters[0].value = point.counters[1].value.clone();
    assert_eq!(point.ratios[0].percent(&point).as_deref(), Some("100.00"));
}

#[test]
fn missing_zero_impossible_mismatched_or_unqualified_inputs_never_become_a_cooked_value() {
    for mutation in [
        "missing",
        "zero-base",
        "outside",
        "object",
        "instance",
        "reset",
        "scope",
        "type",
        "clock",
        "unknown",
        "unsupported",
    ] {
        let mut point = sample();
        match mutation {
            "missing" => {
                point.counters.pop();
                point.ratios[0].base = None;
            }
            "zero-base" => point.counters[1].value = MeasurementValue::Integer("0".into()),
            "outside" => point.counters[1].value = MeasurementValue::Integer("1".into()),
            "object" => point.counters[1].native_object = "another-cache".into(),
            "instance" => point.counters[1].native_instance = Some("another-database".into()),
            "reset" => point.counters[1].reset = crate::ResetMarker::Generation("other".into()),
            "scope" => {
                point.scope = crate::MeasurementScope::Database;
                for row in &mut point.counters {
                    row.scope = point.scope;
                }
            }
            "type" => {
                point.counters[1].nature = CounterNature::Unknown;
                point.counters[1].unit = CounterUnit::Unknown;
            }
            "clock" => point.ratios[0].observed_at = None,
            "unknown" => point.ratios[0].support = CounterRatioSupport::Unknown,
            "unsupported" => point.ratios[0].support = CounterRatioSupport::Unsupported,
            _ => panic!(),
        }
        assert!(point.is_valid(), "{mutation}");
        assert!(point.ratios[0].percent(&point).is_none(), "{mutation}");
        assert!(!point.ratios[0].issues(&point).is_empty(), "{mutation}");
    }
}

#[test]
fn rejected_signed_values_and_absent_source_expectations_are_not_synthetic_measurements() {
    let mut point = sample();
    let raw = point.counters.remove(0);
    point.ratio_gaps.push(CounterRatioGap {
        key: raw.key,
        raw_value: "-1".into(),
        native_object: raw.native_object,
        native_name: raw.native_name,
        native_instance: raw.native_instance,
        native_type: raw.native_type.unwrap(),
    });
    assert!(point.is_valid());
    assert_eq!(point.ratios[0].issues(&point), vec![CounterRatioIssue::InvalidNumerator]);
    assert!(point.ratios[0].percent(&point).is_none());
    point.ratio_gaps.clear();
    point.counters.clear();
    point.properties.insert("read-completed".into(), "true".into());
    point.ratios[0].numerator = None;
    point.ratios[0].base = None;
    assert!(point.is_valid());
    assert_eq!(
        point.ratios[0].issues(&point),
        vec![CounterRatioIssue::MissingNumerator, CounterRatioIssue::MissingBase]
    );
}

#[test]
fn malformed_duplicate_or_unbounded_declarations_fail_original_model_validation() {
    for mutation in ["counter", "pair", "same-key", "clock", "gap", "too-many", "native-type", "read-context"] {
        let mut point = sample();
        match mutation {
            "native-type" => point.counters[0].native_type = Some("42".into()),
            "read-context" => {
                let mut ratio = point.ratios[0].clone();
                ratio.numerator = None;
                ratio.base = None;
                ratio.observed_at = Some("2020-01-01T12:00:01Z".into());
                point.ratios.push(ratio);
            }
            "counter" => point.counters.push(point.counters[0].clone()),
            "pair" => point.ratios.push(point.ratios[0].clone()),
            "same-key" => point.ratios[0].base = point.ratios[0].numerator.clone(),
            "clock" => point.ratios[0].observed_at = Some("2020-01-01T12:00:00.1234567891Z".into()),
            "gap" => point.ratio_gaps.push(CounterRatioGap {
                key: "unreferenced".into(),
                raw_value: "-0".into(),
                native_object: "native-cache".into(),
                native_name: "base".into(),
                native_instance: None,
                native_type: "1073939712".into(),
            }),
            "too-many" => point.ratios = vec![point.ratios[0].clone(); MAX_RATIOS + 1],
            _ => panic!(),
        }
        assert!(!point.is_valid(), "{mutation}");
    }
}

#[test]
fn sharing_preserves_typed_pairing_and_exact_values_without_private_native_labels_or_property_transcripts() {
    let mut point = sample();
    point.identity.id = "private-cache-source-identity".into();
    point.identity.qualifiers.insert("private-qualifier-key".into(), "private-native-endpoint".into());
    point.properties.insert("private-property-key".into(), "private-password-transcript".into());
    for counter in &mut point.counters {
        counter.native_object = "private-counter-object".into();
        counter.native_instance = Some("private-cache-instance".into());
    }
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![point]);
    let shared = crate::redact::redact_snapshot(&snapshot);
    let sets = shared.pressure.as_ref().unwrap().resource_counters.as_ref().unwrap();
    assert!(sets[0].is_valid());
    assert_eq!(sets[0].ratios[0].percent(&sets[0]).as_deref(), Some("99.99"));
    assert_eq!(sets[0].counters[0].value.integer(), Some(9_007_199_254_740_993));
    let json = serde_json::to_string(&sets).unwrap();
    for private in [
        "private-cache-source-identity",
        "private-qualifier-key",
        "private-native-endpoint",
        "private-property-key",
        "private-password-transcript",
        "private-counter-object",
        "private-cache-instance",
    ] {
        assert!(!json.contains(private), "{private}");
    }
    let twice = crate::redact::redact_snapshot(&shared);
    assert_eq!(
        serde_json::to_value(sets).unwrap(),
        serde_json::to_value(&twice.pressure.as_ref().unwrap().resource_counters).unwrap()
    );
}

#[test]
fn sharing_does_not_repair_a_malformed_original_pair_or_hide_its_failure_as_empty_health() {
    let mut point = sample();
    point.ratios[0].base = Some("private-forged-input".into());
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![point]);
    let shared = crate::redact::redact_snapshot(&snapshot);
    assert!(shared.pressure.as_ref().unwrap().resource_counters.is_none());
    assert!(shared.source_problem(&[crate::source_id::PERFORMANCE_COUNTERS]).is_some());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-forged-input"));
}

#[test]
fn a_declaration_cannot_borrow_another_reads_inputs_profile_or_clock() {
    let original = sample();
    let mut later = original.clone();
    later.ratios[0].support = CounterRatioSupport::Unknown;
    later.ratios[0].observed_at = Some("2020-01-01T12:00:01Z".into());
    assert!(later.is_valid());
    assert_eq!(original.ratios[0].percent(&original).as_deref(), Some("99.99"));
    assert!(later.ratios[0].percent(&later).is_none());
    // Native keys are deterministic and may be the same in later reads. They do not transfer the original
    // read's source qualification or clock to the later raw values.
    assert!(original.ratios[0].percent(&later).is_none());
    assert!(original.ratios[0].issues(&later).contains(&CounterRatioIssue::IncompatibleInputs));
}
