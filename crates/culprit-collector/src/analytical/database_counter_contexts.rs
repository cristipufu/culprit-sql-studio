//! Only an authenticated original source can restore the selected counter population's native database proof.
use culprit_core::{Result, telemetry::*};
use serde_json::{Value, json};

pub(super) fn project(identity: &NativeIdentity) -> Result<Value> {
    if !validate_database_performance_identity(identity) {
        return Err(invalid());
    }
    Ok(json!({"key":identity.key()?}))
}
pub(super) fn restore(details: &mut Value, source: &CollectionDescription) -> Result<()> {
    let selected = details.pointer("/properties/performance_profile").and_then(Value::as_str)
        == Some(DATABASE_PERFORMANCE_COUNTER_PROFILE);
    if !selected {
        return Ok(());
    }
    database_performance_context(source).map_err(|_| invalid())?;
    let SourceObservation::DatabasePerformanceCounters { native_database, .. } = &source.observation else {
        return Err(invalid());
    };
    let identity = details.get_mut("identity").ok_or_else(invalid)?;
    if *identity != project(native_database)? {
        return Err(invalid());
    }
    *identity = serde_json::to_value(native_database).map_err(|_| invalid())?;
    Ok(())
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytical::{prepared, test_support};
    fn evidence() -> (CollectionDescription, Value, NativeCounterSet) {
        let (source, records) =
            test_support::database_performance_counter_evidence(test_support::DatabasePerformanceCounterCase::Exact);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(set), .. }) =
            &records[0]
        else {
            panic!()
        };
        let mut input = json!({"record":records[0]});
        prepared::prepare(&mut input, &records[0], &source.target).unwrap();
        (source, input["_projection"]["details"].clone(), *set.clone())
    }
    #[test]
    fn projection_keeps_original_values_and_clocks_but_only_an_opaque_database_reference() {
        let (source, mut details, original) = evidence();
        let public = serde_json::to_string(&details).unwrap();
        for private in [
            "private-database-guid-canary",
            "private-fork-guid-canary",
            "private-file-guid-canary",
            "private-process-canary",
        ] {
            assert!(!public.contains(private));
        }
        assert_eq!(details["identity"], json!({"key":original.identity.key().unwrap()}));
        assert_eq!(details["counters"][0]["value"]["value"], "9007199254740993");
        restore(&mut details, &source).unwrap();
        assert_eq!(serde_json::from_value::<NativeCounterSet>(details).unwrap(), original);
    }
    #[test]
    fn foreign_epoch_native_proof_or_public_identity_injection_cannot_restore_original_database_counters() {
        for cause in ["epoch", "proof", "injection", "generic", "role"] {
            let (mut source, mut details, original) = evidence();
            match cause {
                "epoch" => {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                    *epoch = Some("foreign".into());
                }
                "proof" => {
                    let SourceObservation::DatabasePerformanceCounters { native_database, .. } =
                        &mut source.observation
                    else {
                        panic!()
                    };
                    native_database.qualifiers.insert("database_guid".into(), "foreign".into());
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                    *epoch = Some(native_database.key().unwrap());
                }
                "injection" => details["identity"] = serde_json::to_value(original.identity).unwrap(),
                "generic" => {
                    source.observation = SourceObservation::Snapshot(source.observation.snapshot().unwrap().clone())
                }
                _ => {
                    let SourceObservation::DatabasePerformanceCounters { database_writable, .. } =
                        &mut source.observation
                    else {
                        panic!()
                    };
                    *database_writable = false;
                }
            }
            assert!(restore(&mut details, &source).is_err(), "{cause}");
        }
    }
}
