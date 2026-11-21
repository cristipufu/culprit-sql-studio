use super::*;
fn sample() -> NativeCounterSet {
    let mut point = super::common::sample();
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    point.counters = profile
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::Locks)
        .map(|field| {
            let code = match field.nature {
                CounterNature::Cumulative => 272696576,
                CounterNature::AverageNumerator => 1073874176,
                CounterNature::FractionBase => 1073939712,
                _ => unreachable!(),
            };
            NativeCounterSample {
                key: field.id.into(),
                value: MeasurementValue::Integer("3".into()),
                unit: field.unit,
                nature: field.nature,
                scope: crate::MeasurementScope::Server,
                reset: crate::ResetMarker::Unknown,
                native_object: "SQLServer:Locks".into(),
                native_name: field.native_name.into(),
                native_instance: Some("_Total".into()),
                native_type: Some(code.to_string()),
            }
        })
        .collect();
    point.properties.insert(PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), "6".into());
    point.ratios[0] = NativeCounterRatio {
        kind: CounterRatioKind::LockWaitAverageInputs,
        numerator: Some("detailed_lock_wait_numerator".into()),
        base: Some("detailed_lock_wait_base".into()),
        support: CounterRatioSupport::Supported,
        observed_at: point.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY).cloned(),
    };
    point
}
#[test]
fn only_exact_total_locks_are_requested_and_average_type_has_its_native_millisecond_numerator() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let fields = profile
        .detailed_fields
        .iter()
        .filter(|field| field.family == PerformanceCounterFamily::Locks)
        .collect::<Vec<_>>();
    assert_eq!(
        fields.iter().map(|field| field.native_name).collect::<Vec<_>>(),
        [
            "Lock Requests/sec",
            "Lock Waits/sec",
            "Lock Timeouts/sec",
            "Lock Timeouts (timeout > 0)/sec",
            "Average Wait Time (ms)",
            "Average Wait Time Base"
        ]
    );
    for field in fields {
        assert!(!field.application);
        assert!(profile.detailed_field("MSSQL$NAMED:Locks", field.native_name, Some("_Total")).is_some());
        assert!(profile.detailed_field("SQLServer:Locks", field.native_name, Some("Page")).is_none());
        assert!(profile.detailed_field("SQLServer:Other Locks", field.native_name, Some("_Total")).is_none());
    }
    let numerator = profile.detailed_field("SQLServer:Locks", "Average Wait Time (ms)", Some("_Total")).unwrap();
    assert_eq!(numerator.meaning(1073874176), (CounterNature::AverageNumerator, CounterUnit::Milliseconds));
    for code in [537003264, 65792, 42] {
        assert_eq!(numerator.meaning(code), (CounterNature::Unknown, CounterUnit::Unknown));
    }
    profile.validate(&sample()).unwrap();
}
#[test]
fn an_average_declaration_cannot_borrow_a_percentage_base_or_ordinary_database_population() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    for cause in ["base", "instance", "ordinary", "database", "scope"] {
        let mut point = sample();
        match cause {
            "base" => {
                let row = point.counters.last_mut().unwrap();
                row.native_object = "SQLServer:Buffer Manager".into();
                row.native_name = "Buffer cache hit ratio base".into();
                row.native_instance = None;
            }
            "instance" => point.counters.last_mut().unwrap().native_instance = Some("Page".into()),
            "ordinary" => {
                point
                    .properties
                    .insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), PERFORMANCE_COUNTER_PROFILE.into());
            }
            "database" => {
                point
                    .properties
                    .insert(PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), DATABASE_PERFORMANCE_COUNTER_PROFILE.into());
            }
            "scope" => {
                point.scope = crate::MeasurementScope::Database;
                point.counters.iter_mut().for_each(|row| row.scope = point.scope);
            }
            _ => unreachable!(),
        }
        assert!(profile.validate(&point).is_err(), "{cause}");
    }
}
