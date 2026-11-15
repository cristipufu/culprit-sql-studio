use super::*;
use crate::{
    activity::database_context::tests::row,
    instance::test_support::{epoch, metadata},
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn azure_platform() -> Rec {
    replace(&metadata(), &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)])
}

#[tokio::test]
async fn shared_owner_uses_the_callers_reader_and_keeps_source_scope_separate_from_execution_history() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![row(3)]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![row(3)]),
    ]);
    let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    let after = before.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
    let context = before.completed(&after).unwrap();
    assert!(context.partition.is_qualified());
    assert!(matches!(context.partition, SourcePartition::InstanceLocal { .. }));
    assert_eq!(context.database_writable, Some(true));
    assert_eq!(reader.queries(), 6);
    assert_eq!(reader.closed, 0, "source-context helpers never close or replace their caller's owner");
    assert!(
        reader
            .calls
            .iter()
            .all(|call| !call.sql.contains("sys.dm_exec_requests")
                && !call.sql.contains("sys.query_store_runtime_stats"))
    );
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn equal_missing_member_facts_preserve_an_unqualified_raw_point_without_inventing_an_epoch() {
    let unknown = replace(&row(5), &[("replica_id", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![azure_platform()]), Ok(vec![unknown.clone()]), Ok(vec![unknown])]);
    let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    let after = before.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
    assert!(!before.is_qualified());
    assert!(!before.same_source(&after));
    let context = before.completed(&after).unwrap();
    assert!(!context.partition.is_qualified());
    assert!(matches!(context.partition, SourcePartition::InstanceLocal { member: None, epoch: None }));
    assert_eq!(reader.queries(), 3);
}

#[tokio::test]
async fn observed_member_role_process_and_database_changes_reject_completed_ownership() {
    for (field, value) in [
        ("replica_id", Val::Text("11111111-2222-3333-4444-555555555555".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("local_process_id", Val::Int(12)),
        ("selected_database_id", Val::Int(8)),
    ] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![azure_platform()]),
            Ok(vec![row(5)]),
            Ok(vec![replace(&row(5), &[(field, value)])]),
        ]);
        let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
        let after = before.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
        let error = before.completed(&after).unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.queries(), 3);
    }
}

#[tokio::test]
async fn a_backwards_observation_clock_or_changed_transport_cannot_finish_the_source_context() {
    let backwards = replace(&row(5), &[("observed_at", Val::Text("2020-01-01T05:59:59Z".into()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![azure_platform()]), Ok(vec![row(5)]), Ok(vec![backwards])]);
    let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    let after = before.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
    assert_eq!(CollectionStop::from_error(&before.completed(&after).unwrap_err()), CollectionStop::InvalidPage);

    let mut reader = GenerationalRead {
        inner: FakeRead::new(vec![Ok(vec![azure_platform()]), Ok(vec![row(5)]), Ok(vec![row(5)])]),
        generation: 1,
    };
    let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
    reader.generation = 2;
    let after = before.recheck(&mut reader, "fixture.invalid", 1433).await.unwrap();
    assert_eq!(CollectionStop::from_error(&before.completed(&after).unwrap_err()), CollectionStop::SourceChanged);
}

#[tokio::test]
async fn terminal_native_rechecks_propagate_without_replaying_or_closing_the_owner() {
    for stop in [CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed, CollectionStop::QueryLimit] {
        let mut reader =
            FakeRead::new(vec![Ok(vec![azure_platform()]), Ok(vec![row(5)]), Err(ReadError::Stopped(stop))]);
        let before = Context::read(&mut reader, "fixture.invalid", 1433).await.unwrap();
        let error = before.recheck(&mut reader, "fixture.invalid", 1433).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.closed, 0);
    }
}

struct GenerationalRead {
    inner: FakeRead,
    generation: u64,
}
#[async_trait::async_trait]
impl NativeRead for GenerationalRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.inner.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.inner.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}
