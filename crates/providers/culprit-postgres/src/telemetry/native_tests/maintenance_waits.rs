//! Optional owned concurrent-index cases. Source reads remain read-only; fixture writers are separate.
use super::owned_fixture;
use culprit_core::telemetry::*;

mod capture;
mod fixture;

use super::retained_activity::original;
use capture::progress;
use fixture::Fixture;

#[tokio::test]
#[ignore = "requires the nonce-owned PostgreSQL smoke fixture; select maintenance_waits:: explicitly"]
async fn concurrent_index_wait_is_generation_bound_and_the_reported_locker_remains_unresolved() {
    Fixture::run("waiting", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            fixture.wait_for_writer().await;
            let parent = original(&fixture.specification, fixture.builder_pid, &fixture.cancellation).await;
            let point = progress(&fixture.specification, &parent, &fixture.cancellation).await;
            let source = point.source();
            let SourceObservation::MaintenanceProgress { snapshot, result } = &source.observation else { panic!() };
            assert_eq!(result.binding, FocusedRequestBinding::Matched);
            assert_eq!(result.membership, FocusedRequestMembership::Observed);
            assert_eq!(snapshot.records, 1);
            assert!(!snapshot.sources[0].complete_scope);
            assert!(matches!(source.partition, SourcePartition::InstanceLocal { member: Some(_), .. }));
            assert!(source.partition.native_epoch().is_some());
            assert_eq!(point.records.len(), 1);
            let TelemetryRecord::Operational(row) = &point.records[0].record else { panic!() };
            let OperationalFact::MaintenanceProgress(native) = &row.fact else { panic!() };
            let wait = native.postgres_index_wait().unwrap();
            assert_eq!(native.actor.session.id, fixture.builder_pid.to_string());
            assert_eq!(native.command, MaintenanceCommand::CreateIndexConcurrently);
            assert_eq!(native.phase, "waiting for writers before build");
            assert_eq!(wait.phase, PostgresIndexWaitPhase::WritersBeforeBuild);
            assert_eq!(wait.unresolved_locker_pid, Some(fixture.holder_pid.to_string()));
            let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &native.work else { panic!() };
            assert!(progress.lockers_total.as_deref().is_some_and(|value| value.parse::<u64>().unwrap() >= 1));
            assert_eq!(progress.lockers_done.as_deref(), Some("0"));
            assert!(native.index_reference().is_some());
            assert!(
                point.manifest.source_queries
                    <= crate::progress::bounded_budget(&CollectionBudget::default()).max_queries
            );
            let serialized = serde_json::to_value(wait).unwrap();
            assert!(serialized.get("owner_sql").is_none());
            assert!(serialized.get("owner_generation").is_none());
        })
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the nonce-owned PostgreSQL smoke fixture; select maintenance_waits:: explicitly"]
async fn acknowledged_fixture_completion_does_not_turn_a_missing_native_request_into_completion_evidence() {
    Fixture::run("released", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            fixture.wait_for_writer().await;
            let parent = original(&fixture.specification, fixture.builder_pid, &fixture.cancellation).await;
            let expected_key = parent.focus.original.key.clone();
            fixture.release_and_complete().await;
            // Only this owned fixture observed the CREATE command finish. The retained source does not know its outcome.
            let point = progress(&fixture.specification, &parent, &fixture.cancellation).await;
            let SourceObservation::MaintenanceProgress { snapshot, result } = &point.source().observation else {
                panic!()
            };
            assert_eq!(result.focus.as_ref().unwrap().original.key, expected_key);
            assert_eq!(result.binding, FocusedRequestBinding::ParticipantChanged);
            assert_eq!(result.membership, FocusedRequestMembership::NotObserved);
            assert_eq!(snapshot.records, 0);
            assert!(point.records.is_empty());
            assert!(!snapshot.sources[0].complete_scope);
            assert!(
                point.manifest.source_queries
                    <= crate::progress::bounded_budget(&CollectionBudget::default()).max_queries
            );
        })
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the nonce-owned PostgreSQL smoke fixture; select maintenance_waits:: explicitly"]
async fn an_original_native_activity_receipt_authenticates_the_focused_owner_and_retained_blocking_report() {
    Fixture::run("waiting", |fixture| {
        Box::pin(async move {
            fixture.prepare().await;
            fixture.wait_for_writer().await;
            let mut original = original(&fixture.specification, fixture.builder_pid, &fixture.cancellation).await;
            assert!(original.focus.is_blocking_focus());
            let parent = original.focus.parent.clone();
            let original_key = original.focus.original.key.clone();
            let current = original.blocking(&fixture.specification, &fixture.cancellation).await;
            let SourceObservation::FocusedBlocking { result, .. } = &current.source().observation else { panic!() };
            assert_eq!(result.focus.parent, parent);
            assert_eq!(result.focus.original.key, original_key);
            assert_eq!(result.binding, FocusedRequestBinding::Matched);
            assert_eq!(result.request_end, BlockingRequestState::SameStatement);
            let rows = current
                .records
                .iter()
                .enumerate()
                .map(|(index, exported)| {
                    let TelemetryRecord::Activity(row) = &exported.record else { panic!() };
                    (row.as_ref(), current.reference(index))
                })
                .collect::<Vec<_>>();
            let evidence = rows
                .iter()
                .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
                .collect::<Vec<_>>();
            let report = culprit_checks::focused_blocking::explain(current.source(), &evidence, true).unwrap();
            assert_eq!(report.original, parent);
            assert_eq!(report.binding, FocusedRequestBinding::Matched);
            assert!(report.context.has_qualified_local_owner());
            let holder = fixture.holder_pid.to_string();
            assert!(report.relationships.iter().any(|edge| {
                edge.waiter.session.id == original_key.session.id
                    && edge.owner_observed
                    && matches!(&edge.owner, ActivityWaitOwner::Session { id, generation: Some(_) } if id == &holder)
            }), "the native closure must identify the actual independently held UPDATE backend");
            let owner = rows.iter().find(|(row, _)| row.key.session.id == holder).unwrap().0;
            assert!(owner.key.request.is_none() && owner.statement_identity.is_none());
            assert!(matches!(owner.statement, DefinitionPayload::Unavailable { .. }));
            assert!(!report.relationships.is_empty());
            assert!(
                current.manifest.source_queries
                    <= crate::telemetry::blocking::bounded_budget(&CollectionBudget::default()).max_queries
            );
        })
    })
    .await;
}
