use super::*;
use culprit_collector::analytical::evidence::RecoveryPayloadBudget;

mod lifecycle;
mod profiles;
mod session;

#[test]
fn evidence_requests_require_exact_range_identity_distinct_families_and_valid_budgets() {
    let mut request = EvidenceRequest {
        range_id: culprit_core::telemetry::digest(b"committed-capture"),
        families: vec![EvidenceFamily::OperationalSamples],
        budget: EvidenceReadBudget::default(),
    };
    assert!(request.validate().is_ok());
    request.families.push(EvidenceFamily::OperationalSamples);
    assert!(request.validate().is_err());
    request.families.clear();
    assert!(request.validate().is_err());
    request.families.push(EvidenceFamily::OperationalSamples);
    for range in ["../capture", "capture", "", &"a".repeat(65)] {
        request.range_id = range.into();
        assert!(request.validate().is_err());
    }
    request.range_id = culprit_core::telemetry::digest(b"committed-capture");
    request.budget.page_rows = 0;
    assert!(request.validate().is_err());
    request.budget.page_rows = 1;
    for limits in [
        RecoveryPayloadBudget { max_objects: 0, max_bytes: 1 },
        RecoveryPayloadBudget { max_objects: 129, max_bytes: 1 },
        RecoveryPayloadBudget { max_objects: 1, max_bytes: 0 },
        RecoveryPayloadBudget { max_objects: 1, max_bytes: 256 * 1024 * 1024 + 1 },
    ] {
        request.budget.recovery_payloads = Some(limits);
        assert!(request.validate().is_err());
    }
    request.budget.recovery_payloads = Some(RecoveryPayloadBudget::default());
    assert!(request.validate().is_ok());
}
