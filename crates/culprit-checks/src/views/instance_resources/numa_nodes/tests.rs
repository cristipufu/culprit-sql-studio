use super::*;
#[test]
fn unavailable_original_never_acquires_clock_population_or_aggregate_memory_proof() {
    let report = unavailable(&culprit_core::message!("view.numa_nodes.sharing_withheld"));
    let nodes = report.numa_nodes.unwrap();
    assert!(nodes.context.is_none() && nodes.populations.is_empty());
    assert!(!nodes.read_complete);
    assert!(report.groups.is_empty() && report.observed_at.is_none() && report.native_populations_complete.is_none());
    assert_eq!(report.source_state, SourceState::Unavailable);
}
