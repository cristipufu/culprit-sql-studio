use super::*;

/// The native rows and logical database are identical. Only independently observed member/epoch/role facts vary.
pub fn replica_activity_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (baseline, rows) = activity_evidence_cases().remove(0);
    [
        (Some("private-compute-a"), Some("start-a"), Some(true)),
        (Some("private-compute-b"), Some("start-a"), Some(false)),
        (Some("private-compute-a"), Some("new-start-a"), Some(true)),
        (Some("private-compute-a"), Some("start-a"), Some(false)),
        (None, None, Some(false)),
        (Some("private-compute-a"), None, None),
        (None, Some("start-a"), Some(false)),
    ]
    .into_iter()
    .map(|(member, epoch, writable)| {
        let mut source = baseline.clone();
        source.partition = SourcePartition::InstanceLocal {
            member: member.map(|id| SharedResourceIdentity {
                engine: source.target.engine(),
                kind: SharedResourceKind::Node,
                namespace: NativeIdentity::new("private-compute-namespace"),
                native: NativeIdentity::new(id),
            }),
            epoch: epoch.map(str::to_owned),
        };
        let qualified = source.partition.is_qualified();
        let SourceObservation::Activity(frame) = &mut source.observation else { unreachable!() };
        frame.epoch = epoch.map(str::to_owned);
        frame.database_writable = writable;
        frame.coverage = StateSourceCoverage::observed(source.source.clone(), rows.len() as u32, qualified);
        assert!(frame.is_valid());
        assert!(source.partition_is_consistent());
        (source, rows.clone())
    })
    .collect()
}

/// Replicated history belongs to the declared database, not the route that observed the retained interval.
pub fn replicated_history_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let (source, rows) = query_evidence();
    ["private-compute-a", "private-compute-b"]
        .into_iter()
        .map(|member| {
            let mut source = source.clone();
            source.partition = SourcePartition::Target;
            let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
            history.native_properties.insert("reader_member".into(), member.into());
            (source, rows.clone())
        })
        .collect()
}

pub fn assert_replica_activity_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_activity_capture(capture, source, records);
    assert_eq!(capture.source().partition, source.partition);
    if !source.is_partition_qualified() {
        let frame = capture.source().observation.activity().unwrap();
        assert_eq!(frame.coverage.state, culprit_core::SourceState::Partial);
        assert!(!frame.coverage.complete_scope);
    }
}
