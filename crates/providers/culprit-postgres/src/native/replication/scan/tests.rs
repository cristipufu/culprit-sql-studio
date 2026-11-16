use super::*;
use crate::native::replication::tests::{native_row, sender, slot};
use std::collections::VecDeque;

#[derive(Default)]
struct Reader {
    replies: VecDeque<Result<Vec<Row>>>,
    calls: Vec<String>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> Result<Vec<Row>> {
        self.calls.push(sql.into());
        self.replies.pop_front().expect("unexpected replication query")
    }
}
fn context(standby: bool, full_visibility: bool) -> Row {
    Row::from_fields([
        ("standby".into(), Some(if standby { "t" } else { "f" }.into())),
        ("database_oid".into(), Some("16384".into())),
        ("version".into(), Some("170000".into())),
        ("full_visibility".into(), Some(if full_visibility { "t" } else { "f" }.into())),
    ])
}
fn reader(rows: Vec<Row>, visible: bool) -> Reader {
    Reader { replies: [Ok(vec![context(false, visible)]), Ok(rows)].into(), ..Default::default() }
}

#[tokio::test]
async fn one_existing_acquisition_retains_raw_facts_source_counts_and_legacy_projection_without_reopening() {
    let mut io = reader(
        vec![native_row("pg_stat_replication", "42", sender()), native_row("pg_replication_slots", "consumer", slot())],
        true,
    );
    let acquired = acquire(&mut io, None).await.unwrap();
    assert_eq!(io.calls.len(), 2);
    assert!(io.replies.is_empty());
    assert_eq!(acquired.replicas.len(), 2);
    assert_eq!(acquired.snapshot.observations.len(), 2);
    assert!(matches!(acquired.snapshot.context, ReplicationReadContext::Collector { .. }));
    assert!(
        acquired.snapshot.context.native().is_none(),
        "drained acquisition does not create native member/epoch proof"
    );
    assert!(acquired.snapshot.is_valid());
    assert!(
        acquired
            .snapshot
            .sources
            .iter()
            .all(|source| source.records == 1 && source.state == culprit_core::SourceState::Collected)
    );
    assert!(io.calls[0].contains("server_version_num"), "version comes from this existing reader, not another probe");
}

#[tokio::test]
async fn hidden_or_empty_native_sources_preserve_each_source_visibility_without_healthy_zero() {
    for visible in [false, true] {
        let mut io = reader(vec![], visible);
        let acquired = acquire(&mut io, None).await.unwrap();
        assert!(acquired.snapshot.observations.is_empty());
        assert_eq!(acquired.snapshot.sources.len(), 2);
        for source in &acquired.snapshot.sources {
            assert_eq!(source.records, 0);
            assert_eq!(
                source.state,
                if visible { culprit_core::SourceState::Empty } else { culprit_core::SourceState::Partial }
            );
            assert_eq!(source.complete_scope, visible);
        }
        assert_eq!(io.calls.len(), 2);
    }
    let mut values = sender();
    values["full_visibility"] = serde_json::json!(false);
    let mut io = reader(vec![native_row("pg_stat_replication", "42", values)], true);
    let acquired = acquire(&mut io, None).await.unwrap();
    assert!(!acquired.complete_visibility);
    assert_eq!(acquired.snapshot.sources[0].state, culprit_core::SourceState::Partial);
}

#[tokio::test]
async fn terminal_source_failures_never_fallback_or_publish_a_partial_acquisition() {
    for stop in
        [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::QueryLimit, CollectionStop::ByteLimit]
    {
        let mut io = reader(vec![], true);
        io.replies[1] = Err(stop.error());
        assert_eq!(CollectionStop::from_error(&acquire(&mut io, None).await.err().unwrap()), stop);
        assert_eq!(io.calls.len(), 2);
        assert!(io.replies.is_empty());
    }
}

#[tokio::test]
async fn native_context_changes_and_duplicate_native_rows_do_not_enter_either_projection() {
    for (key, value) in [
        ("read_standby", serde_json::json!(true)),
        ("read_database_oid", serde_json::json!("16385")),
        ("server_version_num", serde_json::json!("160000")),
    ] {
        let mut values = sender();
        values[key] = value;
        let mut io = reader(vec![native_row("pg_stat_replication", "42", values)], true);
        assert_eq!(
            CollectionStop::from_error(&acquire(&mut io, None).await.err().unwrap()),
            CollectionStop::SourceChanged
        );
        assert_eq!(io.calls.len(), 2);
    }
    let same = native_row("pg_replication_slots", "consumer", slot());
    let mut io = reader(vec![same.clone(), same], true);
    assert!(acquire(&mut io, None).await.is_err());
}

#[tokio::test]
async fn foreign_logical_sender_keeps_raw_server_evidence_without_database_name_alias_grading() {
    let mut values = sender();
    values["database"] = serde_json::json!("APPLICATION");
    values["database_oid"] = serde_json::json!("16385");
    let mut io = reader(vec![native_row("pg_stat_replication", "42", values)], true);
    let acquired = acquire(&mut io, None).await.unwrap();
    assert_eq!(acquired.snapshot.observations.len(), 1);
    assert_eq!(acquired.snapshot.sources[0].records, 1);
    assert!(acquired.replicas.is_empty());
}

#[tokio::test]
async fn existing_aurora_standby_refusal_is_explicit_and_sends_no_progress_query() {
    let mut io = Reader { replies: [Ok(vec![context(true, true)])].into(), ..Default::default() };
    let acquired = acquire(&mut io, Some(Platform::AmazonAurora)).await.unwrap();
    assert_eq!(io.calls.len(), 1);
    assert!(io.replies.is_empty());
    assert!(acquired.profile_unavailable);
    assert!(acquired.replicas.is_empty());
    assert!(
        acquired
            .snapshot
            .sources
            .iter()
            .all(|source| source.state == culprit_core::SourceState::NotApplicable && source.records == 0)
    );
}
