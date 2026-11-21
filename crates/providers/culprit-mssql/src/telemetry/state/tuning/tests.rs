use super::super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    query_store::tuning::native::tests::{forced_row, layout_row, option_row},
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, instance_pressure_row, replace},
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, QueryStoreScope, SourceState, source_id};

struct Reader {
    inner: FakeRead,
    generation: Option<u64>,
    change_at: Option<u32>,
}
#[async_trait::async_trait]
impl NativeRead for Reader {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        self.generation.map(|generation| generation + u64::from(self.change_at.is_some_and(|at| self.queries() >= at)))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning },
        budget: CollectionBudget::default(),
    }
}
fn authority(at: &str) -> Rec {
    replace(&crate::activity::database_context::tests::row(3), &[("observed_at", Val::Text(at.into()))])
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    let layout = replace(&layout_row(), &[("runtime_replica", Val::Int(0))]);
    let forced =
        replace(&forced_row(), &[("force_failure_count", Val::Int(0)), ("last_force_failure_reason_desc", Val::Null)]);
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(3))])]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:00Z")]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:00Z")]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![layout]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![option_row()]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![forced]),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:01Z")]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![authority("2020-01-01T06:00:02Z")]),
    ]
}
fn source_with_replies(rows: Vec<ReadResult<Vec<Rec>>>, requested: &CollectionRequest) -> StateSource<Reader> {
    StateSource::with_io(
        Reader {
            inner: FakeRead::new(rows).with_query_limit(tuning_budget(&requested.budget).max_queries),
            generation: Some(1),
            change_at: None,
        },
        requested,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap()
}
async fn drain(source: &mut StateSource<Reader>) -> Vec<TelemetryRecord> {
    let mut records = Vec::new();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::TuningObservations, cursor.as_deref()).await.unwrap()
        else {
            panic!("original Tuning page")
        };
        records.extend(page);
        cursor = next_cursor;
        if cursor.is_none() {
            return records;
        }
    }
}

#[tokio::test]
async fn boxed_tuning_retains_local_database_authority_and_uses_the_existing_native_fact_decoder() {
    let mut source = source_with_replies(replies(), &request());
    let description = source.describe().await.unwrap();
    let owner = NativeReadContext::from_snapshot(&description).unwrap();
    assert!(owner.has_qualified_local_owner());
    assert_eq!(description.target.database().unwrap().incarnation, None);
    assert!(
        matches!(description.partition,SourcePartition::InstanceLocal{member:Some(ref member),..} if member.kind==SharedResourceKind::Instance)
    );
    assert_eq!(description.continuity, SourceContinuity::Unverified);
    let records = drain(&mut source).await;
    let forced = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::ForcedPlan(forced), .. }) => Some(forced),
            _ => None,
        })
        .unwrap();
    assert_eq!(forced.plan_scope, QueryStoreScope::Local);
    assert_eq!(forced.forcing.as_deref(), Some("MANUAL"));
    assert_eq!(forced.failures, "0");
    assert_eq!(forced.reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), super::MAX_READER_QUERIES);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.inner.closed, 1);
    assert_eq!(source.io.inner.calls.iter().filter(|call| call.sql.contains("force_failure_count")).count(), 1);
    assert!(source.io.inner.calls.iter().all(|call| !call.sql.contains("dm_user_db_resource_governance")));
}

#[tokio::test]
async fn completed_permission_failures_preserve_independent_tuning_facts() {
    let mut rows = replies();
    rows[10] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "fixture denied")));
    let mut source = source_with_replies(rows, &request());
    let description = source.describe().await.unwrap();
    let coverage = &description.observation.snapshot().unwrap().sources;
    assert_eq!(coverage[1].state, SourceState::Unavailable);
    assert_eq!(coverage[1].error_code, Some(ErrorCode::PermissionDenied));
    assert!(!coverage[1].complete_scope);
    assert!(
        drain(&mut source).await.iter().any(|row| matches!(
            row,
            TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::ForcedPlan(_), .. })
        ))
    );
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn azure_tuning_keeps_its_existing_guid_physical_primary_and_pressure_authority() {
    let identity = crate::query_store::window_sql::tests::azure_row();
    let layout = replace(&layout_row(), &[("runtime_replica", Val::Int(0))]);
    let pressure = super::super::tests::pressure(false);
    let rows = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity.clone()]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:00Z")]),
        Ok(vec![pressure.clone()]),
        Ok(vec![layout]),
        Ok(vec![pressure.clone()]),
        Ok(vec![option_row()]),
        Ok(vec![pressure.clone()]),
        Ok(vec![]),
        Ok(vec![pressure.clone()]),
        Ok(vec![forced_row()]),
        Ok(vec![pressure]),
        Ok(vec![]),
        Ok(vec![super::super::tests::clock(true, "2020-01-01T06:00:01Z")]),
        Ok(vec![identity.clone()]),
    ];
    let mut original = source_with_replies(rows, &request());
    let description = original.describe().await.unwrap();
    assert_eq!(description.partition, SourcePartition::Target);
    assert_eq!(description.target.database().unwrap().incarnation, identity.str("incarnation"));
    drain(&mut original).await;
    assert_eq!(original.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(original.query_count(), 15);
    assert_eq!(original.close().await, SourceShutdown::Quiescent);
    assert!(original.io.inner.calls.iter().any(|call| call.sql.contains("sys.dm_user_db_resource_governance")));
    assert!(original.io.inner.calls.iter().all(|call| !call.sql.contains("sys.database_recovery_status")));
    for (field, value) in [
        ("incarnation", Val::Null),
        ("physical_incarnation", Val::Null),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut invalid = source_with_replies(
            vec![Ok(vec![Rec::of(&[("edition", Val::Int(5))])]), Ok(vec![replace(&identity, &[(field, value)])])],
            &request(),
        );
        assert!(invalid.describe().await.is_err(), "{field}");
        assert_eq!(invalid.query_count(), 2);
        assert_eq!(invalid.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn cancellation_or_unconfirmed_native_work_stops_siblings_and_preserves_original_shutdown() {
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::Pressure,
    ] {
        let mut rows = replies();
        rows[10] = Err(ReadError::Stopped(stop));
        let mut source = source_with_replies(rows, &request());
        let shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        source.io.inner.shutdown = shutdown;
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), stop);
        assert_eq!(source.query_count(), 11);
        assert_eq!(source.close().await, shutdown);
        assert_eq!(source.io.inner.closed, 1);
    }
}

#[tokio::test]
async fn missing_or_readonly_local_authority_never_dispatches_tuning_inventory() {
    for (field, value) in [
        ("database_guid", Val::Null),
        ("recovery_fork_guid", Val::Null),
        ("updateability", Val::Null),
        ("updateability", Val::Text("READ_ONLY".into())),
    ] {
        let mut rows = replies();
        rows[3] = Ok(vec![replace(&authority("2020-01-01T06:00:00Z"), &[(field, value)])]);
        let mut source = source_with_replies(rows, &request());
        assert!(source.describe().await.is_err(), "{field}");
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut source = source_with_replies(replies(), &request());
    source.io.generation = None;
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn only_the_selected_tuning_profile_can_admit_a_boxed_database_and_shared_tuning_stays_refused() {
    let mut wrong = request();
    wrong.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    let mut source = source_with_replies(vec![Ok(vec![Rec::of(&[("edition", Val::Int(3))])])], &wrong);
    assert_eq!(source.describe().await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut shared = request();
    shared.subject = CollectionSubject::Shared {
        resource: SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-instance"),
            native: NativeIdentity::new("native-process"),
        },
    };
    assert!(StateSource::with_io(FakeRead::new(vec![]), &shared, "fixture.invalid".into(), 1433).is_err());
}

#[tokio::test]
async fn final_database_process_role_clock_or_socket_changes_withhold_source_completion() {
    for (field, value) in [
        ("database_guid", Val::Text("replacement".into())),
        ("database_file_guid", Val::Text("replacement".into())),
        ("recovery_fork_guid", Val::Text("replacement".into())),
        ("selected_database_id", Val::Int(8)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00Z".into())),
    ] {
        let mut rows = replies();
        rows[18] = Ok(vec![replace(&authority("2020-01-01T06:00:02Z"), &[(field, value)])]);
        let mut source = source_with_replies(rows, &request());
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{field}");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
    let mut backwards = replies();
    backwards[4] = Ok(vec![super::super::tests::clock(true, "2019-12-31T23:59:59Z")]);
    let mut backwards = source_with_replies(backwards, &request());
    assert_eq!(CollectionStop::from_error(&backwards.describe().await.unwrap_err()), CollectionStop::SourceChanged);
    assert_eq!(backwards.close().await, SourceShutdown::Quiescent);
    let mut rows = replies();
    rows[17] = Ok(vec![epoch(101)]);
    let mut source = source_with_replies(rows, &request());
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut source = source_with_replies(replies(), &request());
    source.io.change_at = Some(17);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unknown_replica_attribution_remains_a_partial_positive_fact() {
    let mut rows = replies();
    rows[6] = Ok(vec![layout_row()]);
    let mut source = source_with_replies(rows, &request());
    let description = source.describe().await.unwrap();
    let forced = description
        .observation
        .snapshot()
        .unwrap()
        .sources
        .iter()
        .find(|source| source.source == source_id::FORCED_PLANS)
        .unwrap();
    assert_eq!(forced.state, SourceState::Partial);
    assert!(!forced.complete_scope);
    assert!(drain(&mut source).await.iter().any(|row|matches!(row,TelemetryRecord::Tuning(TuningObservation{fact:TuningFact::ForcedPlan(forced),..}) if forced.plan_scope==QueryStoreScope::Unknown)));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[test]
fn native_query_row_byte_and_deadline_limits_intersect_the_callers_independent_bounds() {
    let upper = super::bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(upper.max_queries, 19 + crate::read::ReadSession::SESSION_SETUP_QUERIES);
    assert_eq!(upper.max_snapshot_rows, 128);
    assert_eq!(upper.page_rows, 32);
    assert_eq!(upper.page_bytes, 64 * 1024);
    assert_eq!(upper.total_bytes, 256 * 1024);
    assert_eq!(upper.statement_timeout_ms, 1000);
    assert_eq!(upper.total_timeout_ms, 5000);
    let requested = CollectionBudget {
        max_queries: 7,
        max_snapshot_rows: 4,
        page_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_pages: 3,
        statement_timeout_ms: 20,
        total_timeout_ms: 40,
        ..Default::default()
    };
    assert_eq!(super::bounded_budget(&requested), requested);
}

#[tokio::test]
async fn a_caller_query_cap_cannot_be_extended_to_finish_the_local_authority_check() {
    let mut request = request();
    request.budget.max_queries = 18;
    let mut source = source_with_replies(replies(), &request);
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(CollectionStop::from_error(&source.finish().await.unwrap_err()), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 18);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
