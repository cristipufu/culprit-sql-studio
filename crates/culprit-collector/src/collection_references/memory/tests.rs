use crate::{
    analytical::test_support::focused_request_memory_evidence_with_peers, collection_references::ObservationReferences,
};
use culprit_core::telemetry::*;

#[test]
fn the_original_and_every_declared_peer_are_required_even_when_individual_rows_are_admissible() {
    let (source, records) = focused_request_memory_evidence_with_peers(2);
    for missing in 0..records.len() {
        let mut references = ObservationReferences::default();
        for (index, record) in records.iter().enumerate() {
            if index != missing {
                references.record(record);
            }
        }
        assert!(!references.coverage(true, Some(&source)).complete, "missing row {missing}");
    }
    let mut references = ObservationReferences::default();
    for record in &records {
        references.record(record);
    }
    assert!(references.coverage(true, Some(&source)).complete);
    references.record(&records[1]);
    assert!(!references.coverage(true, Some(&source)).complete, "a repeated peer cannot stand in for another entry");
}

#[test]
fn native_source_and_point_are_not_replaced_by_equal_resource_or_grant_identifiers() {
    let (source, records) = focused_request_memory_evidence_with_peers(1);
    let grants = records
        .iter()
        .map(|record| match record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) => grant.as_ref(),
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
    let subject = grants.iter().find(|row| result.is_subject(row)).unwrap();
    let peer = grants.iter().find(|row| result.is_peer(row)).unwrap();
    for changed in ["source", "point"] {
        let mut other = (**peer).clone();
        if changed == "source" {
            other.source = "foreign-native-source".into();
        } else {
            other.observed_at = "2020-01-01T12:00:01.001Z".into();
        }
        assert!(other.is_valid());
        assert!(!result.matches_observations([*subject, &other]), "{changed}");
    }
}
