use super::*;

#[test]
fn malformed_exact_population_is_rejected_without_a_native_source() {
    for identity in [NativeIdentity::new("07"), NativeIdentity::new("-1"), NativeIdentity::new("other-source")] {
        assert!(crate::query_store::replicas::requested_group(&identity).is_err());
    }
}
