use super::*;
use culprit_core::operations::IndexUsageScope;
use culprit_postgres::wire::{QueryRead, Row};
use std::collections::VecDeque;

const FIRST: &str = "11111111111111111111111111111111";
const SECOND: &str = "22222222222222222222222222222222";

struct Reader {
    replies: VecDeque<Vec<Row>>,
    calls: usize,
    notice_on: Option<usize>,
}

#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, _: &str, _: usize) -> Result<Vec<Row>> {
        self.calls += 1;
        Ok(self.replies.pop_front().expect("only the existing five bounded population reads are allowed"))
    }
}
impl NoticeRead for Reader {
    fn notices(&self) -> u64 {
        u64::from(self.notice_on.is_some_and(|call| self.calls >= call))
    }
}

fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(name, value)| ((*name).into(), value.map(str::to_owned))))
}
fn nodes() -> Vec<Row> {
    [FIRST, SECOND].into_iter().map(|node| row(&[("node", Some(node))])).collect()
}
fn epochs() -> Vec<Row> {
    [FIRST, SECOND]
        .into_iter()
        .map(|node| row(&[("node", Some(node)), ("reset", Some("2026-10-01T00:00:00Z"))]))
        .collect()
}
fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/yugabyte-demo.json"
    )))
    .unwrap();
    snapshot.indexes.truncate(1);
    snapshot.operations.index_usage_scope = Some(IndexUsageScope::AllReadMembers);
    snapshot
}
fn probe(global: bool) -> Probe {
    Probe {
        now: "2026-10-11T00:00:00Z".into(),
        started: Some("2020-01-01T00:00:00Z".into()),
        version: "2025.2.0.0".into(),
        compatibility: 150002,
        database: "fixture".into(),
        oid: 42,
        node: Some(FIRST.into()),
        cluster: global,
        global,
        statements: false,
        statement_schema: None,
        metrics: false,
        rpc_metrics: false,
        statement_role: None,
    }
}
fn reader(snapshot: &Snapshot) -> Reader {
    let index = &snapshot.indexes[0];
    let usage = row(&[
        ("schemaname", Some(&index.schema)),
        ("relname", Some(&index.table)),
        ("indexrelname", Some(&index.name)),
        ("reads", Some("9264")),
        ("nodes", Some("2")),
    ]);
    Reader { replies: vec![nodes(), epochs(), vec![usage], epochs(), nodes()].into(), calls: 0, notice_on: None }
}

#[tokio::test]
async fn a_complete_current_population_preserves_positive_totals_without_proving_historical_membership() {
    let mut snapshot = snapshot();
    snapshot.operations.index_usage_scope = Some(IndexUsageScope::ConnectedMember);
    let mut reader = reader(&snapshot);
    let mut coverage = Coverage::default();
    read(&mut reader, &probe(true), &mut snapshot, &mut coverage).await;
    assert_eq!(reader.calls, 5);
    assert!(reader.replies.is_empty());
    assert_eq!(snapshot.operations.index_usage_scope, Some(IndexUsageScope::ObservedMembers));
    assert_eq!(snapshot.indexes[0].reads, Some(9264.0));
    assert_eq!(snapshot.indexes[0].counters_since.as_deref(), Some("2026-10-01T00:00:00Z"));
    assert!(
        coverage
            .outcomes()
            .iter()
            .any(|source| source.source == sources::INDEX_USAGE.name && source.state == SourceState::Collected)
    );
}

#[tokio::test]
async fn old_epochs_and_zero_reads_on_all_current_members_never_establish_historical_membership() {
    let mut snapshot = snapshot();
    let mut reader = reader(&snapshot);
    let index = &snapshot.indexes[0];
    reader.replies[2][0] = row(&[
        ("schemaname", Some(&index.schema)),
        ("relname", Some(&index.table)),
        ("indexrelname", Some(&index.name)),
        ("reads", Some("0")),
        ("nodes", Some("2")),
    ]);
    let mut coverage = Coverage::default();
    read(&mut reader, &probe(true), &mut snapshot, &mut coverage).await;
    assert_eq!(reader.calls, 5);
    assert!(reader.replies.is_empty());
    assert_eq!(snapshot.indexes[0].reads, Some(0.0));
    assert_eq!(snapshot.indexes[0].counters_since.as_deref(), Some("2026-10-01T00:00:00Z"));
    assert_eq!(snapshot.operations.index_usage_scope, Some(IndexUsageScope::ObservedMembers));
    assert!(
        coverage
            .outcomes()
            .iter()
            .any(|source| source.source == sources::INDEX_USAGE.name && source.state == SourceState::Collected)
    );
}

#[tokio::test]
async fn changed_contributors_epochs_or_partial_index_rows_withdraw_previous_global_scope() {
    for changed in ["inventory", "epoch", "warning", "index-contributors"] {
        let mut snapshot = snapshot();
        let mut reader = reader(&snapshot);
        match changed {
            "inventory" => reader.replies[4].truncate(1),
            "epoch" => {
                reader.replies[3][0] = row(&[("node", Some(FIRST)), ("reset", Some("2026-10-10T00:00:00Z"))]);
            }
            "warning" => reader.notice_on = Some(3),
            _ => {
                let index = &snapshot.indexes[0];
                reader.replies[2][0] = row(&[
                    ("schemaname", Some(&index.schema)),
                    ("relname", Some(&index.table)),
                    ("indexrelname", Some(&index.name)),
                    ("reads", Some("0")),
                    ("nodes", Some("1")),
                ]);
            }
        }
        let mut coverage = Coverage::default();
        read(&mut reader, &probe(true), &mut snapshot, &mut coverage).await;
        assert_eq!(reader.calls, 5);
        assert!(reader.replies.is_empty());
        assert_eq!(snapshot.operations.index_usage_scope, None, "{changed}");
        assert!(
            coverage
                .outcomes()
                .iter()
                .any(|source| source.source == sources::INDEX_USAGE.name && source.state == SourceState::Partial),
            "{changed}"
        );
    }
}

#[tokio::test]
async fn a_local_projection_does_not_inherit_global_scope_from_an_old_snapshot_or_long_uptime() {
    let mut snapshot = snapshot();
    let original_reads = snapshot.indexes[0].reads;
    let mut reader = Reader { replies: VecDeque::new(), calls: 0, notice_on: None };
    let mut coverage = Coverage::default();
    read(&mut reader, &probe(false), &mut snapshot, &mut coverage).await;
    assert_eq!(reader.calls, 0);
    assert_eq!(snapshot.operations.index_usage_scope, Some(IndexUsageScope::ConnectedMember));
    assert_eq!(snapshot.indexes[0].reads, original_reads);
    assert!(
        coverage
            .outcomes()
            .iter()
            .any(|source| source.source == sources::INDEX_USAGE.name && source.state == SourceState::Unavailable)
    );
}
