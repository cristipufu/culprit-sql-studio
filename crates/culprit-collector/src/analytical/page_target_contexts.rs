//! Warehouse references disclose no native member names. Only the authenticated manifest can restore them;
//! reference digests bind content and relationships, not independently measured native authority.
use culprit_core::{Result, telemetry::*};
use serde_json::{Value, json};

pub(super) fn project(context: &NativeReadContext) -> Result<Value> {
    if !(PageTargetScope::SharedResource { context: Box::new(context.clone()) }).is_valid() {
        return Err(invalid());
    }
    let mut value = serde_json::to_value(context).map_err(|_| invalid())?;
    value["target"] = json!({"key":context.target.telemetry_id().map_err(|_|invalid())?});
    value["partition"] = json!({"key":partition_key(&context.partition)?});
    Ok(value)
}

pub(super) fn restore(details: &mut Value, source: &CollectionDescription) -> Result<()> {
    let Some(scope) = details.pointer_mut("/target/scope") else { return Ok(()) };
    match scope.get("kind").and_then(Value::as_str) {
        Some("request-database") => return Ok(()),
        Some("shared-resource") => {}
        _ => return Err(invalid()),
    }
    let projected = scope.get_mut("context").ok_or_else(invalid)?;
    if projected.get("target") != Some(&json!({"key":source.target.telemetry_id().map_err(|_|invalid())?}))
        || projected.get("partition") != Some(&json!({"key":partition_key(&source.partition)?}))
        || !source.is_partition_qualified()
        || !source.partition_is_consistent()
        || !matches!(source.target, CollectionTarget::Shared(_))
    {
        return Err(invalid());
    }
    let mut native = projected.clone();
    native["target"] = serde_json::to_value(&source.target).map_err(|_| invalid())?;
    native["partition"] = serde_json::to_value(&source.partition).map_err(|_| invalid())?;
    let context: NativeReadContext = serde_json::from_value(native).map_err(|_| invalid())?;
    let Some(snapshot) = source.observation.snapshot() else { return Err(invalid()) };
    let Some(bounds) = ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until) else {
        return Err(invalid());
    };
    if !(PageTargetScope::SharedResource { context: Box::new(context.clone()) }).is_valid()
        || !bounds.contains(&context.observed_from)
        || !bounds.contains(&context.observed_until)
        || project(&context)? != *projected
    {
        return Err(invalid());
    }
    *projected = serde_json::to_value(context).map_err(|_| invalid())?;
    Ok(())
}

fn partition_key(partition: &SourcePartition) -> Result<String> {
    serde_json::to_vec(partition).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> (CollectionDescription, Value, PageWaitObservation) {
        let (source, records) = crate::analytical::test_support::shared_page_wait_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(original), .. }) =
            records.into_iter().next().unwrap()
        else {
            panic!()
        };
        let mut details = serde_json::to_value(original.as_ref()).unwrap();
        let PageTargetScope::SharedResource { context } = &original.target.as_ref().unwrap().scope else { panic!() };
        *details.pointer_mut("/target/scope/context").unwrap() = project(context).unwrap();
        (source, details, *original)
    }

    #[test]
    fn opaque_projection_roundtrips_only_through_the_authenticated_source_without_losing_native_clocks_or_role() {
        let (source, mut details, original) = evidence();
        let public = serde_json::to_string(&details).unwrap();
        assert!(!public.contains("private-tempdb-machine-canary"));
        assert!(!public.contains("private-tempdb-instance-canary"));
        let PageTargetScope::SharedResource { context } = &original.target.as_ref().unwrap().scope else { panic!() };
        assert_eq!(details.pointer("/target/scope/context/observed_from").unwrap(), &json!(context.observed_from));
        assert_eq!(details.pointer("/target/scope/context/observed_until").unwrap(), &json!(context.observed_until));
        assert_eq!(
            details.pointer("/target/scope/context/database_writable").unwrap(),
            &json!(context.database_writable)
        );
        restore(&mut details, &source).unwrap();
        assert_eq!(serde_json::from_value::<PageWaitObservation>(details).unwrap(), original);
    }

    #[test]
    fn wrong_owner_epoch_capture_bounds_and_private_context_injection_fail_before_native_row_reconstruction() {
        for (path, value) in [
            ("/target/scope/context/target", json!({"key":digest(b"another owner")})),
            ("/target/scope/context/partition", json!({"key":digest(b"another epoch")})),
            ("/target/scope/context/target", json!({"key":"a","native":"unverified owner"})),
            ("/target/scope/context/observed_from", json!("2019-01-01T12:00:00Z")),
            ("/target/scope/context/observed_until", json!("2021-01-01T12:00:00Z")),
            ("/target/scope/context/database_writable", json!({"invalid":true})),
        ] {
            let (source, mut details, _) = evidence();
            *details.pointer_mut(path).unwrap() = value;
            assert_eq!(
                restore(&mut details, &source).unwrap_err().detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
                "{path}"
            );
        }
    }

    #[test]
    fn a_different_manifest_source_cannot_supply_the_missing_private_member_or_native_epoch() {
        for change in ["owner", "epoch", "unknown-owner"] {
            let (mut source, mut details, _) = evidence();
            match change {
                "owner" => {
                    let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
                    resource.native.id = "another-instance".into();
                }
                "epoch" => {
                    let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                    *epoch = Some("another-process".into());
                }
                "unknown-owner" => {
                    source.partition = SourcePartition::InstanceLocal {
                        member: None,
                        epoch: Some("native-tempdb-process-epoch".into()),
                    }
                }
                _ => unreachable!(),
            }
            assert_eq!(
                restore(&mut details, &source).unwrap_err().detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
                "{change}"
            );
        }
    }
}
