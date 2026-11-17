use super::super::selection::{DefinitionKind, EvidenceSelection};
use super::*;

#[test]
fn compilation_companions_select_query_definitions_and_share_the_original_action_budget() {
    let baseline = culprit_core::telemetry::digest(b"original-compiler-baseline");
    let definitions = culprit_core::telemetry::digest(b"original-query-store-definitions");
    let args = analyze_args(
        "compilation",
        &[
            "--compilation-baseline",
            &baseline,
            "--definitions-range",
            &definitions,
            "--max-queries",
            "7",
            "--max-rows",
            "3",
            "--page-rows",
            "1",
            "--timeout-ms",
            "900",
            "--with-recovery-payloads",
            "--max-recovery-objects",
            "1",
        ],
    );
    let selection = EvidenceSelection::from_command(EvidenceCommand::Analyze(args)).unwrap();
    let request = selection.common.request(selection.plan.families.clone(), Format::Json).unwrap();
    assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
    assert_eq!(request.budget.max_queries, 7);
    assert_eq!(request.budget.max_rows, 3);
    assert_eq!(request.budget.timeout_ms, 900);
    assert_eq!(request.budget.recovery_payloads.unwrap().max_objects, 1);
    assert_eq!(selection.plan.companions.compilation_baseline.as_deref(), Some(baseline.as_str()));
    let companion = selection.plan.companions.definitions.as_ref().unwrap();
    assert_eq!(companion.kind, DefinitionKind::QueryStoreCompilation);
    assert_eq!(companion.kind.family(), EvidenceFamily::QueryDefinitions);
    assert_eq!(companion.range, definitions);
    assert!(selection.plan.companions.activity.is_none() && selection.plan.companions.blocking.is_none());
}

#[test]
fn definition_read_profiles_require_their_actual_native_primary_dataset_and_engine() {
    let case = culprit_collector::analytical::test_support::compilation_definition_evidence_cases().remove(0);
    assert!(DefinitionKind::QueryStoreCompilation.accepts_primary(&case.current.0));
    assert!(!DefinitionKind::CachedStatement.accepts_primary(&case.current.0));
    assert!(!DefinitionKind::QueryStoreCompilation.accepts_primary(&case.definitions.unwrap().0));
    let mut foreign = case.current.0;
    let culprit_core::telemetry::CollectionTarget::Database(database) = &mut foreign.target else { panic!() };
    database.engine = culprit_core::Engine::Postgres;
    assert!(!DefinitionKind::QueryStoreCompilation.accepts_primary(&foreign));
    assert!(!DefinitionKind::CachedStatement.accepts_primary(&foreign));
}

#[tokio::test]
async fn malformed_or_reused_companion_ranges_fail_before_configuration_and_credentials() {
    let original = culprit_core::telemetry::digest(b"archive-population-capture");
    let baseline = culprit_core::telemetry::digest(b"compiler-baseline");
    for definitions in ["../outside", "", original.as_str(), baseline.as_str()] {
        let mut args =
            analyze_args("compilation", &["--compilation-baseline", &baseline, "--definitions-range", definitions]);
        args.common.config = std::env::temp_dir().join("culprit-compilation-definitions-config-must-not-open.json");
        let error = run(EvidenceCommand::Analyze(args), &Out::new(Format::Json)).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::InvalidContract)));
    }
}
