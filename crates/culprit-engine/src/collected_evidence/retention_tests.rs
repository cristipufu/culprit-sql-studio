use super::*;
use culprit_collector::analytical::test_support::query_population_evidence;

// The same sealed recovery/publication/readback fixture serves the public SDK integration tests. Its unrelated
// corruption and hydration controls are intentionally unused in these private small-limit tests.
#[allow(dead_code)]
#[path = "../../tests/collected_evidence/fixture.rs"]
mod fixture;
use fixture::Fixture;

#[tokio::test]
async fn one_explicit_group_fits_while_the_complete_combined_result_exceeds_the_same_limit() {
    let (source, records) = query_population_evidence();
    let population =
        source.query_history().unwrap().population_catalog.as_ref().unwrap().populations[0].identity.clone();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let single = analyze_with_limit(&capture, Some(&population), MAX_RETAINED_ANALYSIS_BYTES).unwrap();
    let limit = serde_json::to_vec(&single).unwrap().len();
    assert_eq!(analyze_with_limit(&capture, Some(&population), limit).unwrap().analyses.len(), 1);
    let opens = fixture.recovery_object_reads();
    let error = analyze_with_limit(&capture, None, limit).unwrap_err();
    assert_eq!(error.code, DeliveryProblem::Capacity.error().code);
    assert_eq!(
        serde_json::to_value(error.detail).unwrap(),
        serde_json::to_value(DeliveryProblem::Capacity.error().detail).unwrap()
    );
    assert_eq!(
        fixture.recovery_object_reads(),
        opens,
        "output accounting performs no additional archive or native reads"
    );
    assert_eq!(error.hint.as_deref(), Some(culprit_core::t!("engine.collected_analysis.output_limit_hint").as_str()));
    let error = serde_json::to_string(&error).unwrap();
    for native in ["SELECT", "public.orders", "fixture-private-literal", "native-history-fixture", "history-group"] {
        assert!(!error.contains(native), "a capacity failure must not serialize discarded native content");
    }
}

#[tokio::test]
async fn exact_combined_boundary_includes_the_context_and_one_array_comma() {
    let (source, records) = query_population_evidence();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let original = analyze_with_limit(&capture, None, MAX_RETAINED_ANALYSIS_BYTES).unwrap();
    assert_eq!(original.analyses.len(), 2);
    let actual = serde_json::to_vec(&original).unwrap().len();
    let exact = analyze_with_limit(&capture, None, actual).unwrap();
    assert_eq!(serde_json::to_vec(&exact).unwrap().len(), actual);
    assert_eq!(exact.context.records.len(), capture.records().len());
    let error = analyze_with_limit(&capture, None, actual - 1).unwrap_err();
    assert_eq!(
        error.code,
        DeliveryProblem::Capacity.error().code,
        "the separator after the first result also consumes one byte"
    );
}

#[tokio::test]
async fn a_limit_for_only_the_derived_sections_cannot_exclude_the_original_evidence_context() {
    let (source, records) = query_population_evidence();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let original = analyze_with_limit(&capture, None, MAX_RETAINED_ANALYSIS_BYTES).unwrap();
    let derived = serde_json::to_vec(&original.analyses).unwrap().len();
    let header = CollectedQueryAnalysis {
        context: crate::collected_context::from_capture(&capture),
        requested_population: None,
        analyses: Vec::new(),
    };
    let header_bytes = serde_json::to_vec(&header).unwrap().len();
    assert!(header_bytes > 2, "the complete proof/native inventory is not an empty result-array placeholder");
    assert!(serde_json::to_vec(&original).unwrap().len() > derived);
    assert_eq!(analyze_with_limit(&capture, None, derived).unwrap_err().code, DeliveryProblem::Capacity.error().code);
    assert_eq!(
        analyze_with_limit(&capture, None, header_bytes - 1).unwrap_err().code,
        DeliveryProblem::Capacity.error().code
    );
}
