use super::*;
use crate::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, OutboxCaptureReader,
};
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, NumaNodeCase, numa_node_evidence},
    outbox::CompletedCaptureBudget,
};
use culprit_core::{limits::OperationGuard, telemetry::*};
#[tokio::test]
async fn original_node_attachment_is_idempotent_and_a_changed_same_identity_withdraws_every_measured_survivor() {
    let (source, rows) = numa_node_evidence(NumaNodeCase::Exact);
    let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let local = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::InstanceResources,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(local) = local else { panic!() };
    let read = local.into_read();
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.identity = None;
    for original in [read.clone(), read.clone()] {
        CapturedScanEvidence::numa_nodes(original).unwrap().attach(&mut snapshot).unwrap();
    }
    assert_eq!(snapshot.operations.numa_nodes.len(), 1);
    let original = culprit_checks::analyze(snapshot.clone());
    let client = serde_json::to_value(crate::reports::display(&original)).unwrap();
    assert_eq!(client["numa_nodes"], serde_json::to_value(&original.numa_nodes).unwrap());
    assert_eq!(
        original.numa_nodes.as_ref().unwrap()[0]
            .numa_nodes
            .as_ref()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .native()
            .unwrap()
            .partition,
        source.partition
    );
    let mut changed = read;
    changed.read_complete = false;
    assert!(CapturedScanEvidence::numa_nodes(changed.clone()).unwrap().attach(&mut snapshot).is_err());
    snapshot.operations.numa_nodes.push(RecordedOperationalRead::Captured { read: changed });
    let raw = snapshot.operations.numa_nodes.clone();
    let report = culprit_checks::analyze(snapshot);
    assert_eq!(report.snapshot.operations.numa_nodes, raw);
    let report = report.numa_nodes.unwrap();
    assert_eq!(report[0].source_state, culprit_core::SourceState::Unavailable);
    assert!(report[0].numa_nodes.as_ref().unwrap().context.is_none());
}
