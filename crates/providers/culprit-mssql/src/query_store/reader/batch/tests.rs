use super::*;
use crate::{
    query_store::{replicas::tests::row as replica, tests::definition_row, window_sql::tests::layout_row},
    read::{Parameter, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn layout() -> Rec {
    replace(&layout_row(), &[("replicas", Val::Int(1))])
}
fn lengths(definition: &Rec) -> Rec {
    Rec::of(
        &definition
            .columns()
            .filter(|(key, _)| matches!(*key, "id" | "source_bytes"))
            .map(|(key, value)| (key, value.clone()))
            .collect::<Vec<_>>(),
    )
}
fn reader(mut replies: Vec<ReadResult<Vec<Rec>>>) -> Reader<FakeRead> {
    let mut initial = vec![Ok(vec![layout()]), Ok(vec![replica(1, 1, None), replica(7, 2, None)])];
    initial.append(&mut replies);
    Reader::new(FakeRead::new(initial))
}
fn identities(definition: &Rec) -> HashMap<String, NativeQueryIdentity> {
    [("3".into(), crate::query_store::decode::query_identity(definition).unwrap())].into()
}

#[tokio::test]
async fn definitions_are_shared_only_for_the_same_native_incarnation_and_payload_limit() {
    let original = definition_row("SELECT 1");
    let changed = replace(&original, &[("context_settings_id", Val::Int(6))]);
    let mut reader = reader(vec![
        Ok(vec![lengths(&original)]),
        Ok(vec![original.clone()]),
        Ok(vec![lengths(&changed)]),
        Ok(vec![changed.clone()]),
        Ok(vec![lengths(&changed)]),
        Ok(vec![changed.clone()]),
    ]);
    reader.prepare_population_batch(1, 60).await.unwrap();
    reader.select_batch_population(Some(&NativeIdentity::new("1"))).await.unwrap();
    let first = reader.query_definitions(&["3".into()], &identities(&original), 256).await.unwrap();
    assert_eq!(first[0].payload.text(), Some("SELECT 1"));
    assert_eq!(reader.queries(), 4);
    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    let shared = reader.query_definitions(&["3".into()], &identities(&original), 256).await.unwrap();
    assert_eq!(shared[0].query, first[0].query);
    assert_eq!(reader.queries(), 4, "the original definition is not fetched once per group");
    let next = reader.query_definitions(&["3".into()], &identities(&changed), 256).await.unwrap();
    assert_ne!(next[0].query, first[0].query, "numeric IDs alone cannot select a cached definition");
    assert_eq!(reader.queries(), 6);
    reader.query_definitions(&["3".into()], &identities(&changed), 128).await.unwrap();
    assert_eq!(reader.queries(), 8, "a payload acquired under another bound is not silently substituted");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}

#[tokio::test]
async fn a_conflicting_native_definition_retires_all_cached_rows_before_any_followup() {
    let original = definition_row("SELECT 1");
    let changed = replace(&original, &[("context_settings_id", Val::Int(6))]);
    let mut reader = reader(vec![Ok(vec![lengths(&changed)]), Ok(vec![changed])]);
    reader.prepare_population_batch(1, 60).await.unwrap();
    reader.select_batch_population(Some(&NativeIdentity::new("1"))).await.unwrap();
    let error = reader.query_definitions(&["3".into()], &identities(&original), 256).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.batch.is_none());
    assert!(reader.query_definitions(&["3".into()], &identities(&original), 256).await.is_err());
    assert!(reader.read_rows("SELECT /* culprit */ forbidden after changed binding").await.is_err());
    assert_eq!(reader.queries(), 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

struct ReconnectingRead {
    native: FakeRead,
}
#[async_trait::async_trait]
impl NativeRead for ReconnectingRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.native.query(sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        self.native.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(if self.native.queries() < 3 { 1 } else { 2 })
    }
    async fn close(&mut self) -> SourceShutdown {
        self.native.close().await
    }
}

#[tokio::test]
async fn a_transport_reconnect_after_native_authority_does_not_inherit_cached_population_or_payload_proof() {
    let definition = definition_row("SELECT 1");
    let native = FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(vec![replica(1, 1, None), replica(7, 2, None)]),
        Ok(vec![lengths(&definition)]),
    ]);
    let mut reader = Reader::new(ReconnectingRead { native });
    let catalog = reader.prepare_population_batch(1, 60).await.unwrap();
    reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.unwrap();
    let error = reader.query_definitions(&["3".into()], &identities(&definition), 256).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.batch.is_none());
    assert_eq!(reader.population_catalog(), Some(&catalog), "raw discovery is retained without enrichment authority");
    assert!(reader.finish_population_batch().await.is_err());
    assert_eq!(reader.queries(), 3, "no payload or copied recheck can authorize the replacement connection");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.native.closed, 1);
}

#[tokio::test]
async fn the_shared_byte_budget_retires_cached_and_native_paths_without_a_per_population_reset() {
    let mut reader = reader(vec![Ok(vec![Rec::of(&[("payload", Val::Text("more than four bytes".into()))])])]);
    reader.prepare_population_batch(1, 60).await.unwrap();
    reader.batch.as_mut().unwrap().budget = culprit_read::ReadBudget::new(MAX_BATCH_ROWS, 4);
    reader.select_batch_population(Some(&NativeIdentity::new("1"))).await.unwrap();
    let error = reader.read_rows("SELECT /* culprit */ bounded payload").await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::ByteLimit);
    assert_eq!(CollectionStop::from_error(reader.retirement_reason().unwrap()), CollectionStop::ByteLimit);
    assert!(reader.select_batch_population(Some(&NativeIdentity::new("7"))).await.is_err());
    assert!(reader.read_rows("SELECT /* culprit */ forbidden fresh population").await.is_err());
    assert_eq!(reader.queries(), 3);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
