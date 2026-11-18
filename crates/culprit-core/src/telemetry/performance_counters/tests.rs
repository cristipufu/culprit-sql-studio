use super::*;
use crate::Engine;

#[test]
fn engine_contracts_are_explicit_and_the_fixed_requested_roster_fits_the_existing_native_ceiling() {
    for engine in Engine::ALL {
        let Some(profile) = engine.spec().performance_counters() else { continue };
        assert_eq!(engine, Engine::Mssql);
        assert_eq!(profile.fields.len(), 34);
        let mut fields = std::collections::BTreeSet::new();
        let mut slots = 0;
        for field in profile.fields {
            assert!(fields.insert(field.id));
            slots += match field.instances {
                PerformanceCounterInstances::Named(instances) => instances.len(),
                _ => 1,
            };
            assert!(!field.native_name.contains('\''));
            if let PerformanceCounterInstances::Named(instances) = field.instances {
                assert!(instances.iter().all(|instance| !instance.contains('\'')));
            }
        }
        assert_eq!(slots, 59);
        assert!(slots <= MAX_NATIVE_COUNTERS);
        assert_eq!(profile.fields.iter().filter(|field| field.application).count(), 6);
        let log = profile.fields.iter().find(|field| field.id == "log_bytes_flushed").unwrap();
        assert!(log.matches("SQLServer:Databases", log.native_name, Some("_Total")));
        assert!(!log.matches("SQLServer:Databases", log.native_name, Some("app")));
    }
}

#[test]
fn native_types_authorize_semantics_and_not_the_counter_suffix() {
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let field = profile.fields.iter().find(|field| field.id == "compilations").unwrap();
    assert_eq!(field.meaning(272_696_576), (CounterNature::Cumulative, CounterUnit::Count));
    assert_eq!(field.meaning(65_792), (CounterNature::Unknown, CounterUnit::Unknown));
    assert_eq!(field.meaning(42), (CounterNature::Unknown, CounterUnit::Unknown));
    let gauge = profile.fields.iter().find(|field| field.id == "grants_pending").unwrap();
    assert_eq!(gauge.meaning(65_792), (CounterNature::Gauge, CounterUnit::Count));
    assert_eq!(gauge.meaning(272_696_576), (CounterNature::Unknown, CounterUnit::Unknown));
}
