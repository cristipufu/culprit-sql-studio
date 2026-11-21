use super::*;
use std::collections::BTreeMap;
fn sample(kind: MssqlNumaNodeKind) -> NativeCounterSet {
    let mut sample = NativeCounterSet {
        identity: NativeIdentity::new("0").qualified("native_object", kind.object()),
        scope: crate::MeasurementScope::Server,
        counters: vec![],
        properties: BTreeMap::new(),
        ratios: vec![],
        ratio_gaps: vec![],
    };
    for field in kind.fields() {
        match field.kind {
            MssqlNativeFieldKind::Identity => (),
            MssqlNativeFieldKind::IntegerProperty => {
                sample.properties.insert(
                    field.name.into(),
                    if field.name.ends_with("mask") { i64::MIN.to_string() } else { "0".into() },
                );
            }
            MssqlNativeFieldKind::TextProperty => {
                sample.properties.insert(field.name.into(), "ONLINE DAC".into());
            }
            MssqlNativeFieldKind::BooleanProperty => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            MssqlNativeFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
                key: field.name.into(),
                value: MeasurementValue::Integer("9007199254740993".into()),
                unit,
                nature,
                scope: sample.scope,
                reset: crate::ResetMarker::Unknown,
                native_object: kind.object().into(),
                native_name: field.name.into(),
                native_instance: None,
                native_type: None,
            }),
            MssqlNativeFieldKind::UnqualifiedCounter { .. } => unreachable!(),
        }
    }
    sample
}
#[test]
fn documented_populations_have_twenty_three_fields_and_preserve_signed_properties_and_exact_gauges() {
    assert_eq!(MssqlNumaNodeKind::Cpu.fields().len(), 14);
    assert_eq!(MssqlNumaNodeKind::Memory.fields().len(), 9);
    for kind in MssqlNumaNodeKind::ALL {
        let sample = sample(kind);
        let point = MssqlNumaNodePoint::from_sample(&sample).unwrap();
        assert_eq!(point.kind(), kind);
        assert!(
            sample
                .counters
                .iter()
                .all(|value| value.nature == CounterNature::Gauge && value.reset == crate::ResetMarker::Unknown)
        );
        let restored: NativeCounterSet = serde_json::from_slice(&serde_json::to_vec(&sample).unwrap()).unwrap();
        assert!(MssqlNumaNodePoint::from_sample(&restored).is_ok());
        if kind == MssqlNumaNodeKind::Cpu {
            assert_eq!(sample.properties["cpu_affinity_mask"], i64::MIN.to_string())
        }
    }
}
#[test]
fn scalar_zero_native_null_and_unavailable_column_never_collapse() {
    let kind = MssqlNumaNodeKind::Memory;
    let mut sample = sample(kind);
    sample.counters.retain(|value| !matches!(value.key.as_str(), "target_kb" | "pages_kb"));
    sample.properties.insert("unavailable_columns".into(), r#"["target_kb"]"#.into());
    sample.properties.insert("native_null_columns".into(), r#"["pages_kb"]"#.into());
    sample.counters[0].value = MeasurementValue::Integer("0".into());
    let point = MssqlNumaNodePoint::from_sample(&sample).unwrap();
    let field = |name| kind.fields().iter().find(|field| field.name == name).unwrap();
    assert!(matches!(point.field(field("target_kb")).unwrap(), MssqlNativeFieldValue::ColumnUnavailable));
    assert!(matches!(point.field(field("pages_kb")).unwrap(), MssqlNativeFieldValue::NativeNull));
    assert!(matches!(
        point.field(field("virtual_address_space_reserved_kb")).unwrap(),
        MssqlNativeFieldValue::Observed("0")
    ));
}
#[test]
fn names_do_not_override_units_owner_reset_integer_precision_or_complete_roster() {
    for change in [
        "scope",
        "object",
        "unit",
        "nature",
        "reset",
        "fraction",
        "float",
        "negative",
        "leading-zero",
        "extra",
        "missing",
        "overlap",
        "node-id",
    ] {
        let mut sample = sample(MssqlNumaNodeKind::Memory);
        match change {
            "scope" => sample.scope = crate::MeasurementScope::Database,
            "object" => {
                sample.identity.qualifiers.insert("native_object".into(), "sys.dm_os_nodes".into());
            }
            "unit" => sample.counters[0].unit = CounterUnit::Count,
            "nature" => sample.counters[0].nature = CounterNature::Cumulative,
            "reset" => sample.counters[0].reset = crate::ResetMarker::Generation("unproved".into()),
            "fraction" => sample.counters[0].value = MeasurementValue::Integer("1.00000000000000001".into()),
            "float" => sample.counters[0].value = MeasurementValue::Real(1.0),
            "negative" => sample.counters[0].value = MeasurementValue::Integer("-1".into()),
            "leading-zero" => sample.counters[0].value = MeasurementValue::Integer("01".into()),
            "extra" => {
                sample.properties.insert("unrelated-label".into(), "x".into());
            }
            "missing" => {
                sample.counters.remove(0);
            }
            "overlap" => {
                sample.counters.retain(|value| value.key != "pages_kb");
                sample.properties.insert("unavailable_columns".into(), r#"["pages_kb"]"#.into());
                sample.properties.insert("native_null_columns".into(), r#"["pages_kb"]"#.into());
            }
            _ => sample.identity.id = "32768".into(),
        }
        assert!(MssqlNumaNodePoint::from_sample(&sample).is_err(), "{change}");
    }
}
#[test]
fn a_valid_point_cannot_answer_a_different_descriptor_with_a_matching_field_name() {
    let sample = sample(MssqlNumaNodeKind::Memory);
    let point = MssqlNumaNodePoint::from_sample(&sample).unwrap();
    let mut field = *MssqlNumaNodeKind::Memory.fields().iter().find(|field| field.name == "pages_kb").unwrap();
    field.kind = MssqlNativeFieldKind::Counter { unit: CounterUnit::Bytes, nature: CounterNature::Gauge };
    assert!(point.field(&field).is_err());
}
