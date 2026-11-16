use super::*;
use culprit_core::{Engine, StatementTemporaryWork};
use culprit_postgres::wire::QueryRead;
use std::collections::VecDeque;

const NODE: &str = "0123456789abcdef0123456789abcdef";
const STARTED: &str = "2026-11-15T00:00:00Z";

fn row(fields: &[(&str, Option<&str>)]) -> Row {
    Row::from_fields(fields.iter().map(|(name, value)| ((*name).into(), value.map(str::to_owned))))
}

fn probe() -> Probe {
    Probe {
        now: "2026-11-16T10:00:00Z".into(),
        started: Some(STARTED.into()),
        version: "2025.2.0.0".into(),
        compatibility: 150002,
        database: "shop".into(),
        oid: 16384,
        node: Some(NODE.into()),
        cluster: false,
        global: false,
        statements: true,
        statement_schema: Some("public".into()),
        statement_info: true,
        metrics: false,
        rpc_metrics: false,
        statement_role: None,
    }
}

fn columns() -> Vec<Row> {
    [
        "queryid",
        "userid",
        "dbid",
        "query",
        "calls",
        "rows",
        "total_exec_time",
        "min_exec_time",
        "max_exec_time",
        "mean_exec_time",
        "stddev_exec_time",
        "toplevel",
        "temp_blks_read",
        "temp_blks_written",
        "temp_blk_read_time",
        "temp_blk_write_time",
    ]
    .into_iter()
    .map(|name| row(&[("attname", Some(name))]))
    .collect()
}

fn epoch(node: &str, started: &str) -> Vec<Row> {
    vec![row(&[("reset", None), ("node", Some(node)), ("started", Some(started))])]
}

fn statement(changes: &[(&str, Option<&str>)]) -> Row {
    let mut values = BTreeMap::from([
        ("id", Some("-42")),
        ("text", Some("select value from sample")),
        ("calls", Some("2")),
        ("ms", Some("10")),
        ("rows", Some("1")),
        ("min_ms", Some("5")),
        ("max_ms", Some("5")),
        ("moment", Some("50")),
        ("all_ms", Some("10")),
        ("all_calls", Some("2")),
        ("all_queries", Some("1")),
        ("native_rows", Some("1")),
        ("native_userid", Some("10")),
        ("native_toplevel", Some("true")),
        ("temp_blks_read", Some("0")),
        ("temp_blks_written", Some("9007199254740993")),
        ("temp_blk_read_time", None),
        ("temp_blk_write_time", Some("0")),
        ("temp_block_bytes", Some("8192")),
    ]);
    values.extend(changes.iter().copied());
    Row::from_fields(values.into_iter().map(|(name, value)| (name.into(), value.map(str::to_owned))))
}

struct Reader {
    replies: VecDeque<culprit_core::Result<Vec<Row>>>,
    calls: Vec<String>,
    notice_on: Option<usize>,
}
#[async_trait::async_trait]
impl QueryRead for Reader {
    async fn query(&mut self, sql: &str, _: usize) -> culprit_core::Result<Vec<Row>> {
        self.calls.push(sql.into());
        self.replies.pop_front().expect("no additional native acquisition for temporary-work evidence")
    }
}
impl NoticeRead for Reader {
    fn notices(&self) -> u64 {
        u64::from(self.notice_on.is_some_and(|call| self.calls.len() >= call))
    }
}
fn reader(statement: Row) -> Reader {
    Reader {
        replies: [Ok(epoch(NODE, STARTED)), Ok(columns()), Ok(vec![statement]), Ok(epoch(NODE, STARTED))].into(),
        calls: vec![],
        notice_on: None,
    }
}

#[tokio::test]
async fn local_single_native_row_reuses_the_existing_four_reads_and_preserves_exact_temporary_work() {
    let mut reader = reader(statement(&[]));
    let mut coverage = Coverage::default();
    let result = read(&mut reader, &probe(), 5, &mut coverage).await.unwrap();
    assert_eq!(reader.calls.len(), 4);
    assert!(reader.replies.is_empty());
    assert!(
        reader.calls[0].contains("yb_get_local_tserver_uuid") && reader.calls[3].contains("pg_postmaster_start_time")
    );
    let query = &result.queries[0];
    assert_eq!(query.id, "-42", "native plan lookups retain their existing ID");
    assert_eq!(query.executions, 2.0);
    let identity = query.native_identity.as_ref().unwrap();
    assert_eq!(identity.qualifiers["node"], NODE);
    assert_eq!(identity.qualifiers["database_oid"], "16384");
    assert_eq!(identity.qualifiers["userid"], "10");
    assert_eq!(identity.qualifiers["toplevel"], "true");
    assert_eq!(
        Engine::Yugabyte.spec().statement_counter_member(identity),
        culprit_core::engines::ysql_node_member(NODE)
    );
    let StatementTemporaryWork::BlockIo { read_blocks, written_blocks, read_time_ms, write_time_ms, .. } =
        query.temporary_work.as_ref().unwrap()
    else {
        panic!()
    };
    assert_eq!(read_blocks.as_deref(), Some("0"));
    assert_eq!(written_blocks.as_deref(), Some("9007199254740993"));
    assert_eq!(*read_time_ms, None);
    assert_eq!(*write_time_ms, Some(0.0));
    assert_eq!(result.window_start, None, "temporary work does not manufacture a reset or interval");
}

#[tokio::test]
async fn multiple_contexts_unknown_or_changed_members_processes_and_partial_reads_withhold_new_attribution() {
    for changed in ["multiple-rows", "missing-member", "missing-process", "member", "process", "warning"] {
        let mut probe = probe();
        let mut reader = reader(statement(&[]));
        match changed {
            "multiple-rows" => {
                reader.replies[2] = Ok(vec![statement(&[
                    ("native_rows", Some("2")),
                    ("native_userid", None),
                    ("native_toplevel", None),
                ])])
            }
            "missing-member" => probe.node = None,
            "missing-process" => probe.started = None,
            "member" => reader.replies[3] = Ok(epoch("11111111111111111111111111111111", STARTED)),
            "process" => reader.replies[3] = Ok(epoch(NODE, "2026-11-16T09:00:00Z")),
            _ => reader.notice_on = Some(3),
        }
        let result = read(&mut reader, &probe, 5, &mut Coverage::default()).await.unwrap();
        assert_eq!(reader.calls.len(), 4, "{changed}");
        assert_eq!(result.queries[0].executions, 2.0, "existing positive runtime remains retained");
        assert!(result.queries[0].temporary_work.is_none(), "{changed}");
        assert!(result.queries[0].native_identity.is_none(), "{changed}");
    }
}

#[tokio::test]
async fn malformed_temporary_work_withdraws_only_that_projection_and_records_partial_coverage() {
    let mut reader = reader(statement(&[("temp_blks_written", Some("1.5"))]));
    let mut coverage = Coverage::default();
    let result = read(&mut reader, &probe(), 5, &mut coverage).await.unwrap();
    assert_eq!(result.queries[0].executions, 2.0);
    assert!(result.queries[0].temporary_work.is_none());
    assert!(
        coverage
            .outcomes()
            .iter()
            .any(|outcome| outcome.source == sources::WORKLOAD.name
                && outcome.state == culprit_core::SourceState::Partial)
    );
}

#[tokio::test]
async fn detected_projection_never_returns_a_multi_population_temp_sum_or_an_unsupported_timer() {
    let mut reader =
        Reader { replies: [Ok(columns()), Ok(vec![statement(&[])])].into(), calls: vec![], notice_on: None };
    let mut global = probe();
    global.cluster = true;
    statement_rows(&mut reader, &global, "pg_catalog.\"gv$pg_stat_statements\"", 5).await.unwrap();
    let sql = &reader.calls[1];
    assert!(sql.contains("NULL::text AS temp_blks_read"));
    assert!(!sql.contains("sum(temp_blks") && !sql.contains("min(temp_blks"));
    assert!(single_identity(&statement(&[]), &global).is_none());
    let mut reader =
        Reader { replies: [Ok(columns()), Ok(vec![statement(&[])])].into(), calls: vec![], notice_on: None };
    statement_rows(&mut reader, &probe(), "public.pg_stat_statements", 5).await.unwrap();
    assert!(reader.calls[1].contains("CASE WHEN count(*)=1 THEN min(temp_blks_written)::text"));
    assert!(reader.calls[1].contains("CASE WHEN count(*)=1 THEN min(toplevel::text) END"));
}

#[tokio::test]
async fn legacy_layout_and_restricted_optional_reset_metadata_keep_qualified_native_work_without_an_interval() {
    for legacy in [false, true] {
        let mut probe = probe();
        probe.statement_info = false;
        let mut reader = reader(statement(&[]));
        if legacy {
            probe.version = "2024.2.0.0".into();
            probe.compatibility = 110002;
            let native_columns = columns()
                .into_iter()
                .filter_map(|column| {
                    let name = column.text("attname").unwrap();
                    if matches!(name.as_str(), "toplevel" | "temp_blk_read_time" | "temp_blk_write_time") {
                        None
                    } else {
                        let name = name.replace("_exec_time", "_time");
                        Some(row(&[("attname", Some(&name))]))
                    }
                })
                .collect();
            reader.replies[1] = Ok(native_columns);
            reader.replies[2] = Ok(vec![statement(&[("native_toplevel", None), ("temp_blk_write_time", None)])]);
        }
        let mut coverage = Coverage::default();
        let workload = read(&mut reader, &probe, 5, &mut coverage).await.unwrap();
        assert_eq!(reader.calls.len(), 4, "optional reset metadata adds no replacement query");
        for bound in [0, 3] {
            assert!(reader.calls[bound].contains("NULL::text AS reset"));
            assert!(reader.calls[bound].contains("yb_get_local_tserver_uuid"));
            assert!(reader.calls[bound].contains("pg_postmaster_start_time"));
            assert!(!reader.calls[bound].contains("pg_stat_statements_info"));
        }
        let query = &workload.queries[0];
        assert_eq!(query.native_identity.as_ref().unwrap().qualifiers.contains_key("toplevel"), !legacy);
        let StatementTemporaryWork::BlockIo { written_blocks, read_time_ms, write_time_ms, .. } =
            query.temporary_work.as_ref().unwrap()
        else {
            panic!()
        };
        assert_eq!(written_blocks.as_deref(), Some("9007199254740993"));
        assert_eq!(*read_time_ms, None);
        if legacy {
            assert_eq!(*write_time_ms, None);
            assert!(reader.calls[2].contains("sum(total_time::float8)"));
            assert!(reader.calls[2].contains("NULL::text AS temp_blk_write_time"));
        }
        assert!(workload.window_start.is_none());
        assert_eq!(workload.window_hours, 0.0);
        assert!(!workload.counter_set.as_ref().unwrap().epochs_known());
        assert!(coverage.outcomes().iter().any(
            |outcome| outcome.source == sources::EPOCHS.name && outcome.state == culprit_core::SourceState::Partial
        ));
    }
}

#[tokio::test]
async fn absent_reset_metadata_never_bypasses_either_native_member_or_process_guard() {
    for bound in [0, 3] {
        for changed in ["member", "process"] {
            let mut probe = probe();
            probe.statement_info = false;
            let mut reader = reader(statement(&[]));
            reader.replies[bound] = Ok(if changed == "member" {
                epoch("11111111111111111111111111111111", STARTED)
            } else {
                epoch(NODE, "2026-11-16T09:00:00Z")
            });
            let workload = read(&mut reader, &probe, 5, &mut Coverage::default()).await.unwrap();
            assert_eq!(reader.calls.len(), 4);
            assert!(workload.queries[0].temporary_work.is_none(), "{bound}:{changed}");
            assert!(workload.queries[0].native_identity.is_none(), "{bound}:{changed}");
        }
    }
}

#[tokio::test]
async fn typed_terminal_stops_cannot_trigger_optional_reset_fallback_or_followup_native_work() {
    use culprit_core::telemetry::CollectionStop;
    for info_available in [false, true] {
        for at in 0..4 {
            for stop in [
                CollectionStop::Cancelled,
                CollectionStop::CancellationUnconfirmed,
                CollectionStop::TimedOut,
                CollectionStop::QueryLimit,
                CollectionStop::ByteLimit,
                CollectionStop::SourceChanged,
            ] {
                let mut probe = probe();
                probe.statement_info = info_available;
                let mut reader = reader(statement(&[]));
                reader.replies[at] = Err(stop.error());
                let result = read(&mut reader, &probe, 5, &mut Coverage::default()).await;
                assert!(result.is_none(), "{info_available}:{at}:{stop:?}");
                assert_eq!(reader.calls.len(), at + 1, "{info_available}:{at}:{stop:?}");
                assert_eq!(reader.replies.len(), 3 - at, "no retry or unplanned owner query");
            }
        }
    }
}
