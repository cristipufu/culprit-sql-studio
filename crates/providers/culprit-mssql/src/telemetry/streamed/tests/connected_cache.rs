use super::*;
use crate::telemetry::retained_definitions::{RetainedPlans, bounded_budget};

#[derive(Clone, Copy)]
enum Owner {
    Stable,
    Restarted,
    Hidden,
}

fn source(edition: i64, owner: Owner) -> StreamedSnapshot<RetainedPlans, StreamingRead> {
    source_with_authority(edition, owner, crate::activity::database_context::tests::row(edition))
}

fn source_with_authority(edition: i64, owner: Owner, authority: Rec) -> StreamedSnapshot<RetainedPlans, StreamingRead> {
    let metadata = replace(&crate::instance::test_support::metadata(), &[("edition", Val::Int(edition))]);
    let epoch = || match owner {
        Owner::Hidden => Err(crate::read::ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "owned test epoch is hidden",
        ))),
        _ => Ok(vec![crate::instance::test_support::epoch(100)]),
    };
    let row = crate::cached_definitions::tests::row();
    let binding = crate::relational_inputs::tests::binding_row(&crate::cached_statements::context(&row).unwrap());
    let after_metadata = match owner {
        Owner::Restarted => replace(&metadata, &[("process_id", Val::Int(43))]),
        _ => metadata.clone(),
    };
    let io = StreamingRead {
        native: FakeRead::new(vec![
            Ok(vec![metadata]),
            epoch(),
            Ok(vec![authority.clone()]),
            Ok(vec![crate::resources::test_support::instance_pressure_row()]),
            Ok(vec![crate::retained_definitions::tests::feature(1, 1)]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![crate::resources::test_support::instance_pressure_row()]),
            Ok(vec![binding]),
            Ok(vec![after_metadata]),
            epoch(),
            Ok(vec![authority]),
        ]),
        batches: vec![batch(row, true)].into(),
        starts: 0,
        pause_batches: false,
    };
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans },
        budget: bounded_budget(&CollectionBudget::default()),
    };
    StreamedSnapshot::with_io(io, &request, "fixture.invalid".into(), 1433, RetainedPlans).unwrap()
}

#[tokio::test]
async fn retained_cache_profiles_use_genuine_instance_authority_without_a_global_database_namespace() {
    for edition in [2, 3, 4, 8] {
        let mut source = source(edition, Owner::Stable);
        let initial = source.describe().await.unwrap();
        assert!(initial.target.database().unwrap().incarnation.is_none());
        assert!(matches!(initial.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) }));
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert!(next_cursor.is_none());
        let completion = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        assert!(completion.observation.snapshot().unwrap().sources[0].complete_scope);
        assert_eq!(source.query_count(), 12, "the boxed/MI authority roster is independent of the Azure roster");
        assert_eq!(source.inner.reader().io.starts, 1);
        assert!(
            source
                .inner
                .reader()
                .io
                .native
                .calls
                .iter()
                .all(|call| !call.sql.contains("sys.dm_user_db_resource_governance"))
        );
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn hidden_native_epoch_stays_partial_without_parameter_authority_and_restart_rejects_completion() {
    let mut hidden = source(3, Owner::Hidden);
    let initial = hidden.describe().await.unwrap();
    assert!(!initial.partition.is_qualified());
    let CollectionRead::Page { records, .. } =
        hidden.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap()
    else {
        panic!()
    };
    let TelemetryRecord::ObjectDefinition(record) = &records[0] else { panic!() };
    let ObjectDefinitionFact::MssqlRetainedPlan(plan) = &record.fact else { panic!() };
    assert!(plan.parameter_context_proof.is_none());
    assert!(plan.relational_input_proof.is_none());
    let completion = initial.completed(hidden.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(completion.observation.snapshot().unwrap().sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(hidden.close().await, SourceShutdown::Quiescent);

    let mut changed = source(3, Owner::Restarted);
    changed.describe().await.unwrap();
    changed.read_page(TelemetryStream::ObjectDefinitions, None).await.unwrap();
    assert!(matches!(changed.finish().await.unwrap(), SourceCompletion::Changed));
    assert_eq!(changed.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn retained_cache_policy_keeps_primary_and_native_platform_gates_before_payload_work() {
    for edition in [2, 3, 4, 8] {
        let readonly = replace(
            &crate::activity::database_context::tests::row(edition),
            &[("updateability", Val::Text("READ_ONLY".into()))],
        );
        let mut source = source_with_authority(edition, Owner::Stable, readonly);
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 3);
        assert_eq!(source.inner.reader().io.starts, 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    for edition in [6, 9, 99] {
        let mut source = source(edition, Owner::Stable);
        assert_eq!(source.describe().await.unwrap_err().code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(source.query_count(), 1);
        assert_eq!(source.inner.reader().io.starts, 0);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[test]
fn retained_source_budget_is_bounded_and_preserves_stricter_caller_limits() {
    let ceiling = bounded_budget(&CollectionBudget::upper_limits());
    ceiling.validate().unwrap();
    assert_eq!((ceiling.page_rows, ceiling.max_snapshot_rows, ceiling.max_queries), (64, 64, 12));
    assert_eq!((ceiling.statement_timeout_ms, ceiling.total_timeout_ms), (1_000, 5_000));
    let strict = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        max_queries: 1,
        statement_timeout_ms: 100,
        total_timeout_ms: 100,
        ..CollectionBudget::default()
    };
    let bounded = bounded_budget(&strict);
    assert_eq!((bounded.page_rows, bounded.max_snapshot_rows, bounded.max_queries), (1, 1, 1));
    assert_eq!((bounded.statement_timeout_ms, bounded.total_timeout_ms), (100, 100));
}
