use super::*;
use culprit_collector::analytical::test_support::{InvestigationFixture, investigation_fixture};

/// This is a fixture-specific native-work policy, not production qualification or common counter-name inference.
fn native_verifier(calls: Arc<AtomicUsize>) -> RecipeCostVerifier {
    Arc::new(move |receipt, source, record| {
        calls.fetch_add(1, Ordering::SeqCst);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(point), .. }) =
            &record.record
        else {
            return Ok(false);
        };
        Ok(source.source == "fixture-native-cost"
            && point.identity.id == "owned-native-observer"
            && point.counters.iter().any(|counter| {
                counter.native_object == "native-observer"
                    && counter.native_name == "logical_reads"
                    && counter.unit == CounterUnit::Count
                    && matches!(&counter.value, MeasurementValue::Integer(value) if value == "12")
            })
            && receipt.read_metrics.templates.iter().any(|metric| {
                metric.template_id == digest(b"investigation-fixture-native-query") && metric.completed_tasks == 2
            }))
    })
}

fn selected(
    fixture: &InvestigationFixture,
    factories: Arc<AtomicUsize>,
    verifiers: Arc<AtomicUsize>,
) -> ProviderRegistration {
    registration(factories, vec![fixture.recipe.clone()])
        .unwrap()
        .with_qualification_authority(fixture.intent.qualification.component.clone(), native_verifier(verifiers))
        .unwrap()
}

#[test]
fn a_measured_catalog_without_its_selected_authority_cannot_approve_any_native_work() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let registration = registration(factories.clone(), vec![fixture.recipe]).unwrap();
    assert!(registration.qualification_component().is_none());
    assert!(!registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn exact_selected_native_cost_facts_reach_the_pure_verifier_without_opening_a_provider() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let registration = selected(&fixture, factories.clone(), verifiers.clone());
    assert_eq!(registration.qualification_component(), Some(&fixture.intent.qualification.component));
    assert!(registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    assert_eq!(verifiers.load(Ordering::SeqCst), 1);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn receipt_identity_component_budget_hash_and_ordinal_mismatches_never_reach_a_verifier() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let registration = selected(&fixture, factories.clone(), verifiers.clone());
    for changed in ["recipe", "version", "component", "revision", "budget", "hash", "ordinal", "accounting"] {
        let mut receipt = fixture.intent.qualification.clone();
        match changed {
            "recipe" => receipt.recipe_id = "another-source.page-wait".into(),
            "version" => receipt.recipe_version += 1,
            "component" => receipt.component.component = "another-native-reader".into(),
            "revision" => receipt.component.revision = Some("different-native-reader-build".into()),
            "budget" => receipt.tested_budget.max_queries += 1,
            "hash" => receipt.evidence.payload_sha256 = digest(b"different-native-artifact"),
            "ordinal" => receipt.evidence.row_ordinal += 1,
            _ => receipt.read_metrics.templates[0].client_elapsed_ms = 2.0,
        }
        assert!(receipt.is_valid(), "{changed}: syntax alone is not authority");
        assert!(!registration.verify_cost(&receipt, &fixture.source, &fixture.record).unwrap(), "{changed}");
    }
    assert_eq!(verifiers.load(Ordering::SeqCst), 0);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_artifact_source_target_native_payload_capture_or_export_format_is_rejected_before_callback() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let registration = selected(&fixture, factories.clone(), verifiers.clone());
    for changed in ["source", "target", "payload", "capture", "format"] {
        let mut source = fixture.source.clone();
        let mut record = fixture.record.clone();
        match changed {
            "source" => source.source = "another-native-observer".into(),
            "target" => {
                source.target = culprit_core::DatabaseIdentity::server(
                    fixture.source.target.engine(),
                    "db",
                    "another-native-host.invalid",
                    1433,
                    None,
                )
                .into();
            }
            "payload" => {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::CounterSet(point),
                    ..
                }) = &mut record.record
                else {
                    panic!()
                };
                point.counters[0].value = MeasurementValue::Integer("1".into());
            }
            "capture" => record.capture.revision.as_mut().unwrap().sequence = "2".into(),
            _ => record.format_version += 1,
        }
        assert!(!registration.verify_cost(&fixture.intent.qualification, &source, &record).unwrap(), "{changed}");
    }
    assert_eq!(verifiers.load(Ordering::SeqCst), 0);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn absent_unqualified_or_different_current_catalogs_do_not_inherit_a_receipts_authority() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let mut unqualified = fixture.recipe.clone();
    unqualified.cost = RecipeCostQualification::Unqualified;
    let mut another = fixture.recipe.clone();
    another.id = "another-source.page-wait-focus".into();
    let RecipeCostQualification::Measured(receipt) = &mut another.cost else { panic!() };
    receipt.recipe_id = another.id.clone();
    for recipes in [vec![], vec![unqualified], vec![another]] {
        let registration = registration(factories.clone(), recipes)
            .unwrap()
            .with_qualification_authority(
                fixture.intent.qualification.component.clone(),
                native_verifier(verifiers.clone()),
            )
            .unwrap();
        assert!(!registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    }
    assert_eq!(verifiers.load(Ordering::SeqCst), 0);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn a_registered_pure_verifier_can_reject_or_fail_without_native_construction() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    for fail in [false, true] {
        let calls = verifiers.clone();
        let registration = registration(factories.clone(), vec![fixture.recipe.clone()])
            .unwrap()
            .with_qualification_authority(
                fixture.intent.qualification.component.clone(),
                Arc::new(move |_, _, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if fail {
                        Err(Error::new(ErrorCode::QueryFailed, "native-cost proof cannot be verified"))
                    } else {
                        Ok(false)
                    }
                }),
            )
            .unwrap();
        let result = registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record);
        if fail {
            assert_eq!(result.unwrap_err().code, ErrorCode::QueryFailed);
        } else {
            assert!(!result.unwrap());
        }
    }
    assert_eq!(verifiers.load(Ordering::SeqCst), 2);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn valid_client_accounting_and_a_matching_reference_cannot_replace_native_cost_measurements() {
    let mut fixture = investigation_fixture();
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CounterSet(point), .. }) =
        &mut fixture.record.record
    else {
        panic!()
    };
    point.counters.clear();
    point.properties.insert("client_elapsed_ms".into(), "1.25".into());
    assert!(fixture.record.record.is_valid());
    let reference = DiagnosticEvidenceRef::from_export(
        &fixture.source,
        &fixture.intent.qualification.evidence.manifest_sha256,
        &fixture.record,
        fixture.intent.qualification.evidence.row_ordinal,
    )
    .unwrap();
    fixture.intent.qualification.evidence = reference;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.recipe.cost else { panic!() };
    **receipt = fixture.intent.qualification.clone();
    assert!(fixture.recipe.is_valid());
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let registration = selected(&fixture, factories.clone(), verifiers.clone());
    assert!(!registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    assert_eq!(verifiers.load(Ordering::SeqCst), 1, "only the source can interpret its measured native cost facts");
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_components_and_mismatched_selected_authorities_cannot_approve_a_receipt() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    for missing in ["component", "version", "revision", "empty-revision"] {
        let mut component = fixture.intent.qualification.component.clone();
        match missing {
            "component" => component.component.clear(),
            "version" => component.version = None,
            "revision" => component.revision = None,
            _ => component.revision = Some(String::new()),
        }
        let error = registration(factories.clone(), vec![fixture.recipe.clone()])
            .unwrap()
            .with_qualification_authority(component, native_verifier(verifiers.clone()))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }
    let mut different = fixture.intent.qualification.component.clone();
    different.revision = Some("another-selected-native-implementation".into());
    let registration = registration(factories.clone(), vec![fixture.recipe.clone()])
        .unwrap()
        .with_qualification_authority(different, native_verifier(verifiers.clone()))
        .unwrap();
    assert!(!registration.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    assert_eq!(verifiers.load(Ordering::SeqCst), 0);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

#[test]
fn replacing_a_selected_registration_does_not_borrow_the_old_cost_verifier() {
    let fixture = investigation_fixture();
    let factories = Arc::new(AtomicUsize::new(0));
    let verifiers = Arc::new(AtomicUsize::new(0));
    let directory = tempfile::tempdir().unwrap();
    let engine = fixture.intent.focus.target.engine();
    let workspace = Workspace::open_in(directory.path())
        .with_provider(engine, selected(&fixture, factories.clone(), verifiers.clone()))
        .with_provider(engine, registration(factories.clone(), vec![fixture.recipe.clone()]).unwrap());
    let selected = &workspace.providers.iter().find(|(selected, _)| *selected == engine).unwrap().1;
    assert!(selected.qualification_component().is_none());
    assert!(!selected.verify_cost(&fixture.intent.qualification, &fixture.source, &fixture.record).unwrap());
    assert_eq!(verifiers.load(Ordering::SeqCst), 0);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}
