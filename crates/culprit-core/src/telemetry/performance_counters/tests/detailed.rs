use super::*;

fn sample() -> NativeCounterSet {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let mut sample = super::common::sample();
    sample.ratios.clear();
    sample.counters = profile
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::AccessMethods)
        .map(|field| NativeCounterSample {
            key: field.id.into(),
            value: MeasurementValue::Integer("9007199254740993".into()),
            unit: CounterUnit::Count,
            nature: CounterNature::Cumulative,
            scope: crate::MeasurementScope::Server,
            reset: crate::ResetMarker::Unknown,
            native_object: "SQLServer:Access Methods".into(),
            native_name: field.native_name.into(),
            native_instance: None,
            native_type: Some("272696576".into()),
        })
        .collect();
    sample.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "6".into());
    sample
}
#[test]
fn detailed_access_descriptors_are_exact_bounded_informational_inputs_with_no_ratio_or_application_projection() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    assert_eq!(
        profile
            .detailed_fields
            .iter()
            .filter(|field| field.family == PerformanceCounterFamily::AccessMethods)
            .map(|field| field.native_name)
            .collect::<Vec<_>>(),
        [
            "Full Scans/sec",
            "Index Searches/sec",
            "Forwarded Records/sec",
            "Page Splits/sec",
            "Workfiles Created/sec",
            "Worktables Created/sec"
        ]
    );
    let slots = profile
        .fields
        .iter()
        .chain(profile.common_fields)
        .chain(profile.detailed_fields)
        .map(|field| match field.instances {
            PerformanceCounterInstances::Named(instances) => instances.len(),
            _ => 1,
        })
        .sum::<usize>();
    assert_eq!(slots, 95);
    assert!(slots <= MAX_NATIVE_COUNTERS);
    for field in profile.detailed_fields.iter().filter(|field| field.family == PerformanceCounterFamily::AccessMethods)
    {
        assert!(matches!(field.instances, PerformanceCounterInstances::Uninstanced));
        assert_eq!(field.family, PerformanceCounterFamily::AccessMethods);
        assert!(!field.application && field.ratio.is_none());
        assert_eq!(field.meaning(272696576), (CounterNature::Cumulative, CounterUnit::Count));
        assert_eq!(field.meaning(272696320), (CounterNature::Cumulative, CounterUnit::Count));
        for code in [65792, 42] {
            assert_eq!(field.meaning(code), (CounterNature::Unknown, CounterUnit::Unknown));
        }
        assert!(profile.detailed_field("MSSQL$NAMED:Access Methods", field.native_name, None).is_some());
        assert!(profile.detailed_field("SQLServer:Access Methods other", field.native_name, None).is_none());
        assert!(profile.detailed_field("SQLServer:Other Access Methods", field.native_name, None).is_none());
        assert!(profile.detailed_field("SQLServer:Access Methods", field.native_name, Some("application")).is_none());
    }
}
#[test]
fn detailed_rows_cannot_borrow_ordinary_database_or_resource_governor_authority() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let sample = sample();
    profile.validate(&sample).unwrap();
    for declared in [
        None,
        Some(PERFORMANCE_COUNTER_PROFILE),
        Some(DATABASE_PERFORMANCE_COUNTER_PROFILE),
        Some(MSSQL_RESOURCE_GOVERNOR_COUNTER_PROFILE),
    ] {
        let mut changed = sample.clone();
        if let Some(declared) = declared {
            changed.properties.insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), declared.into());
        } else {
            changed.properties.remove(PERFORMANCE_COUNTER_PROFILE_PROPERTY);
        }
        assert!(profile.validate(&changed).is_err(), "{declared:?}");
    }
}
#[test]
fn detailed_type_scope_reset_duplicate_and_missing_counter_population_remain_distinct() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    for cause in ["unknown-type", "scope", "reset", "duplicate", "missing"] {
        let mut changed = sample();
        match cause {
            "unknown-type" => {
                let row = &mut changed.counters[0];
                row.native_type = Some("42".into());
                row.nature = CounterNature::Unknown;
                row.unit = CounterUnit::Unknown;
            }
            "scope" => changed.counters[0].scope = crate::MeasurementScope::Database,
            "reset" => changed.counters[0].reset = crate::ResetMarker::Generation("invented".into()),
            "duplicate" => changed.counters[1] = changed.counters[0].clone(),
            "missing" => {
                changed.counters.pop();
                changed.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "5".into());
            }
            _ => unreachable!(),
        }
        assert_eq!(profile.validate(&changed).is_ok(), matches!(cause, "unknown-type" | "missing"), "{cause}");
    }
}
