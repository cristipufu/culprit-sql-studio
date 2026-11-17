use super::*;
use culprit_core::ConnectionSpec;

pub(super) struct Original {
    pub focus: ActivityRequestFocus,
    _capture: owned_fixture::Captured,
}

pub(super) async fn original(specification: &ConnectionSpec, pid: i32) -> Original {
    let captured = owned_fixture::capture_parent(
        specification,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Activity,
            budget: CollectionBudget::default(),
        },
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

pub(super) async fn progress(specification: &ConnectionSpec, original: &Original) -> owned_fixture::Captured {
    owned_fixture::capture(
        specification,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::MaintenanceProgress { focus: Box::new(original.focus.clone()) },
            budget: crate::progress::bounded_budget(&CollectionBudget::default()),
        },
    )
    .await
}
