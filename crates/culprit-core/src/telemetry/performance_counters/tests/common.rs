use super::*;
fn sample() -> NativeCounterSet {
    let object = "SQLServer:Buffer Manager";
    let counters = [
        ("hits", "Buffer cache hit ratio", 537003264, CounterNature::FractionNumerator, "3"),
        ("base", "Buffer cache hit ratio base", 1073939712, CounterNature::FractionBase, "4"),
    ]
    .into_iter()
    .map(|(key, name, code, nature, value)| NativeCounterSample {
        key: key.into(),
        value: MeasurementValue::Integer(value.into()),
        unit: CounterUnit::Count,
        nature,
        scope: crate::MeasurementScope::Server,
        reset: crate::ResetMarker::Unknown,
        native_object: object.into(),
        native_name: name.into(),
        native_instance: None,
        native_type: Some(code.to_string()),
    })
    .collect();
    NativeCounterSet {
        identity: NativeIdentity::new("sys.dm_os_performance_counters")
            .qualified("native_object", "sys.dm_os_performance_counters"),
        scope: crate::MeasurementScope::Server,
        counters,
        ratio_gaps: vec![],
        ratios: vec![NativeCounterRatio {
            kind: CounterRatioKind::BufferCacheHit,
            numerator: Some("hits".into()),
            base: Some("base".into()),
            support: CounterRatioSupport::Supported,
            observed_at: Some("2020-01-01T12:00:00.500000000Z".into()),
        }],
        properties: [
            (PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), MSSQL_COMMON_COUNTER_PROFILE.into()),
            (PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "2".into()),
            (PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(), "2020-01-01T12:00:00.500000000Z".into()),
        ]
        .into(),
    }
}
#[test]
fn common_roster_is_explicit_opt_in_and_keeps_the_existing_default_and_application_contract() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    assert_eq!(profile.fields.len(), 34);
    assert_eq!(profile.fields.iter().filter(|field| field.application).count(), 6);
    assert_eq!(profile.common_fields.len(), 20);
    assert!(profile.common_fields.iter().all(|field| !field.application));
    assert!(profile.common_field("SQLServer:Transactions", "Free Space in tempdb (KB)", None).is_some());
    assert!(profile.common_field("SQLServer:Databases", "Free Space in tempdb (KB)", Some("tempdb")).is_none());
    assert!(profile.common_field("SQLServer:Databases", "Transactions/sec", Some("app")).is_none());
    assert!(profile.common_field("SQLServer:Databases", "Transactions/sec", Some("_Total")).is_some());
    let mut keys = std::collections::BTreeSet::new();
    for field in profile.fields.iter().chain(profile.common_fields).chain(profile.investigation_fields) {
        assert!(keys.insert(field.id));
    }
}
#[test]
fn a_common_fraction_cannot_be_borrowed_by_an_ordinary_or_database_profile() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let original = sample();
    profile.validate(&original).unwrap();
    assert_eq!(original.ratios[0].percent(&original).as_deref(), Some("75.00"));
    for declared in [None, Some(PERFORMANCE_COUNTER_PROFILE), Some(DATABASE_PERFORMANCE_COUNTER_PROFILE)] {
        let mut changed = original.clone();
        if let Some(declared) = declared {
            changed.properties.insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), declared.into());
        } else {
            changed.properties.remove(PERFORMANCE_COUNTER_PROFILE_PROPERTY);
        }
        assert!(profile.validate(&changed).is_err(), "{declared:?}");
    }
}
#[test]
fn same_read_base_scope_clock_and_native_type_are_independent_requirements() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let original = sample();
    for field in ["object", "instance", "scope", "clock", "type", "base"] {
        let mut changed = original.clone();
        match field {
            "object" => changed.counters[1].native_object = "other:Buffer Manager".into(),
            "instance" => changed.counters[1].native_instance = Some("other".into()),
            "scope" => changed.counters[1].scope = crate::MeasurementScope::Database,
            "clock" => changed.ratios[0].observed_at = None,
            "type" => {
                changed.counters[1].native_type = Some("65792".into());
                changed.counters[1].nature = CounterNature::Unknown;
                changed.counters[1].unit = CounterUnit::Unknown;
            }
            "base" => changed.counters[1].value = MeasurementValue::Integer("0".into()),
            _ => unreachable!(),
        }
        assert!(profile.validate(&changed).is_err() || changed.ratios[0].percent(&changed).is_none(), "{field}");
    }
}
