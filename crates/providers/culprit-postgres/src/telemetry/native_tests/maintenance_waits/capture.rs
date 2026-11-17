use super::super::retained_activity::Original;
use super::*;
use culprit_collector::capture::CollectionCancellation;
use culprit_core::ConnectionSpec;

pub(super) async fn progress(
    specification: &ConnectionSpec,
    original: &Original,
    cancellation: &CollectionCancellation,
) -> owned_fixture::Captured {
    owned_fixture::capture(
        specification,
        CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::MaintenanceProgress { focus: Box::new(original.focus.clone()) },
            budget: crate::progress::bounded_budget(&CollectionBudget::default()),
        },
        cancellation,
    )
    .await
}
