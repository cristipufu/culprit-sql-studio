//! One actual sealed Activity acquisition supplies an original receipt shared by focused native tests.
use super::owned_fixture;
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{ConnectionSpec, telemetry::*};

pub(super) struct Original {
    pub focus: ActivityRequestFocus,
    _capture: owned_fixture::Captured,
}

pub(super) async fn original(
    specification: &ConnectionSpec,
    pid: i32,
    cancellation: &CollectionCancellation,
) -> Original {
    let captured = owned_fixture::capture_parent(
        specification,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: CollectionBudget::default(),
        },
        cancellation,
    )
    .await;
    let matches = captured
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, exported)| {
            let TelemetryRecord::Activity(row) = &exported.record else { return None };
            (row.key.session.id == pid.to_string()).then_some((index, row))
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "one original native backend in the actual Activity capture");
    let (index, row) = matches[0];
    let source = captured.source();
    let frame = source.observation.activity().unwrap();
    let focus = ActivityRequestFocus {
        parent: captured.reference(index),
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: frame.database_writable,
        source: source.source.clone(),
        epoch: frame.epoch.clone().unwrap(),
        original_observed_at: frame.at.clone(),
        original: row.as_ref().clone(),
        statement_identity: row.statement_identity.clone().unwrap(),
    };
    assert!(focus.matches_parent(source, row, &captured.reference(index)));
    assert_eq!(focus.partition, SourcePartition::Target);
    Original { focus, _capture: captured }
}
