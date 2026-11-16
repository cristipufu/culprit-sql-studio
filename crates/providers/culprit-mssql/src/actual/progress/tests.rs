use super::*;
use crate::{
    read::{ReadResult, invalid_data},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::telemetry::SourceShutdown;

const XML: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT id FROM orders" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10" ActualExecutions="1" /></RunTimeInformation><TableScan><Object Table="[orders]"/></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

fn layout(live: bool, profiles: bool) -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Int(1)),
        ("last_actual", Val::Int(1)),
        ("live_xml", Val::Int(i64::from(live))),
        ("profiles", Val::Int(i64::from(profiles))),
    ])
}
fn request() -> Rec {
    Rec::of(&[
        ("request_count", Val::Int(1)),
        ("ordinal", Val::Int(0)),
        ("observed_at", Val::Text("2020-01-01T12:00:00Z".into())),
        ("checked_session_id", Val::Int(42)),
        ("checked_session_generation", Val::Text("2020-01-01T10:00:00.000".into())),
        ("checked_request_id", Val::Int(0)),
        ("checked_request_generation", Val::Text("2020-01-01T11:00:00.000".into())),
        ("checked_sql_handle", Val::Text("AB".repeat(64))),
        ("checked_begin", Val::Int(0)),
        ("checked_end", Val::Int(-1)),
        ("checked_database_id", Val::Int(7)),
        ("checked_wait_type", Val::Null),
        ("checked_wait_resource", Val::Null),
        ("checked_plan_handle", Val::Text("CD".repeat(64))),
        ("checked_query_hash", Val::Text("0123456789ABCDEF".into())),
        ("checked_plan_hash", Val::Text("EF".repeat(8))),
        ("selected_database_id", Val::Int(7)),
        ("selected_database_name", Val::Text("app".into())),
        ("reader_session_id", Val::Int(90)),
        ("reader_session_generation", Val::Text("2020-01-01T09:00:00.000".into())),
        ("database_updateability", Val::Text("READ_WRITE".into())),
        ("status", Val::Text("running".into())),
        ("command", Val::Text("SELECT".into())),
        ("wait_type", Val::Null),
        ("wait_time", Val::Int(0)),
        ("elapsed_ms", Val::Int(500)),
        ("cpu_ms", Val::Int(20)),
        ("logical_reads", Val::Int(100)),
        ("percent_complete", Val::Float(0.0)),
        ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
        ("statement_text", Val::Text("SELECT id FROM orders".into())),
        ("live_plan", Val::Text(XML.into())),
        ("cached_plan", Val::Text(XML.into())),
    ])
}
fn guard(changes: &[(&str, Val)]) -> Rec {
    let row = request();
    let mut values = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    values.push(("row_kind", Val::Text("guard".into())));
    let row = replace(&Rec::of(&values), &[("observed_at", Val::Text("2020-01-01T12:00:00.500Z".into()))]);
    replace(&row, changes)
}
fn profile(node: i64) -> Rec {
    Rec::of(&[
        ("row_kind", Val::Text("profile".into())),
        ("node_id", Val::Int(node)),
        ("row_count", Val::Int(10)),
        ("estimate_row_count", Val::Int(100)),
        ("elapsed_ms", Val::Int(500)),
        ("cpu_ms", Val::Int(20)),
        ("logical_reads", Val::Int(100)),
        ("end_of_scans", Val::Int(0)),
        ("replays", Val::Int(0)),
        ("closed", Val::Int(0)),
    ])
}
async fn observe(reader: &mut FakeRead) -> Result<QueryProgress> {
    present("42", read(reader, 42).await?)
}

#[tokio::test]
async fn one_selected_request_uses_native_generations_handles_offsets_and_the_guarded_operator_batch() {
    let mut io =
        FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), Ok(vec![profile(0), guard(&[])])]);
    let progress = observe(&mut io).await.unwrap();
    assert_eq!(progress.running, Some(true));
    assert!(progress.plan.is_some());
    assert_eq!(progress.nodes.len(), 1);
    assert_eq!(progress.nodes[0].rows, 10.0);
    assert!(progress.request_key.is_none());
    assert_eq!(io.calls.len(), 3);
    let sql = &io.calls[1].sql;
    assert!(
        sql.contains("TOP(2)")
            && sql.contains("r.database_id=DB_ID()")
            && sql.contains("s.is_user_process=1")
            && sql.contains("r.session_id<>@@SPID")
    );
    assert!(
        sql.contains("sx.request_id=r.request_id")
            && sql.contains("sx.sql_handle=r.sql_handle")
            && sql.contains("sx.plan_handle=r.plan_handle")
    );
    let sql = &io.calls[2].sql;
    for required in [
        "r.request_id=p.request_id",
        "r.sql_handle=p.sql_handle",
        "r.plan_handle=p.plan_handle",
        "r.start_time=CONVERT(datetime,@P4,126)",
        "s.login_time=CONVERT(datetime,@P3,126)",
        "r.statement_start_offset=@P7",
        "r.statement_end_offset=@P8",
        "'guard' AS row_kind",
    ] {
        assert!(sql.contains(required), "{required}");
    }
    assert_eq!(io.calls[2].parameters.len(), 9);
}

#[tokio::test]
async fn empty_or_multiple_mars_requests_with_reused_node_ids_never_choose_a_representative() {
    for count in [0, 2] {
        let row =
            Rec::of(&[("request_count", Val::Int(count)), ("observed_at", Val::Text("2020-01-01T12:00:00Z".into()))]);
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row])]);
        let progress = observe(&mut io).await.unwrap();
        assert_eq!(progress.running, None);
        assert!(progress.plan.is_none() && progress.nodes.is_empty() && progress.statement.is_none());
        assert_eq!(io.calls.len(), 2);
    }
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request(), request()])]);
    assert!(observe(&mut io).await.is_err());
    assert_eq!(io.calls.len(), 2);
}

#[tokio::test]
async fn reused_session_request_plan_statement_or_scope_withholds_all_progress_even_with_no_profile_rows() {
    for changes in [
        vec![("checked_session_generation", Val::Text("2020-01-01T10:01:00.000".into()))],
        vec![("checked_request_generation", Val::Text("2020-01-01T11:01:00.000".into()))],
        vec![("checked_request_id", Val::Int(1))],
        vec![("checked_sql_handle", Val::Text("EF".repeat(64)))],
        vec![("checked_plan_handle", Val::Text("EF".repeat(64)))],
        vec![("checked_begin", Val::Int(2))],
        vec![("checked_database_id", Val::Int(8))],
        vec![("request_count", Val::Int(0)), ("checked_request_id", Val::Null)],
        vec![("request_count", Val::Int(2))],
        vec![("reader_session_id", Val::Int(91))],
        vec![("reader_session_generation", Val::Text("2020-01-01T09:01:00.000".into()))],
        vec![("observed_at", Val::Text("2020-01-01T11:59:59Z".into()))],
        vec![("database_updateability", Val::Text("READ_ONLY".into()))],
        vec![("selected_database_name", Val::Text("another".into()))],
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), Ok(vec![guard(&changes)])]);
        let progress = observe(&mut io).await.unwrap();
        assert_eq!(progress.running, None);
        assert!(
            progress.plan.is_none()
                && progress.nodes.is_empty()
                && progress.statement.is_none()
                && progress.request_key.is_none()
        );
        assert_eq!(io.calls.len(), 3);
    }
}

#[tokio::test]
async fn a_missing_guard_or_duplicate_operator_cannot_certify_a_partially_read_native_point() {
    for rows in [vec![], vec![profile(0)], vec![guard(&[]), guard(&[])], vec![profile(0), profile(0), guard(&[])]] {
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), Ok(rows)]);
        assert!(observe(&mut io).await.is_err());
        assert_eq!(io.calls.len(), 3);
    }
}

#[tokio::test]
async fn known_absent_profiles_or_unknown_role_keep_only_independent_request_metadata() {
    for (profiles, role) in [(false, Val::Text("READ_WRITE".into())), (true, Val::Null)] {
        let row = replace(&request(), &[("database_updateability", role)]);
        let mut io = FakeRead::new(vec![Ok(vec![layout(false, profiles)]), Ok(vec![row])]);
        let progress = observe(&mut io).await.unwrap();
        assert_eq!(progress.running, Some(true));
        assert!(progress.statement.is_some());
        assert!(progress.plan.is_none() && progress.nodes.is_empty() && progress.request_key.is_none());
        assert_eq!(io.calls.len(), 2);
        assert!(!io.calls[1].sql.contains("dm_exec_query_statistics_xml"));
    }
}

#[tokio::test]
async fn missing_duplicate_or_foreign_native_xml_statement_never_uses_the_pasted_plan_fallback() {
    let another = XML.replace("0x0123456789ABCDEF", "0x9999999999999999");
    let duplicate = XML.replace(
        "</Statements>",
        &format!("{} </Statements>", XML.split("<Statements>").nth(1).unwrap().split("</Statements>").next().unwrap()),
    );
    for xml in [another, duplicate] {
        let row = replace(&request(), &[("live_plan", Val::Text(xml.clone())), ("cached_plan", Val::Text(xml))]);
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row]), Ok(vec![profile(0), guard(&[])])]);
        let progress = observe(&mut io).await.unwrap();
        assert_eq!(progress.running, Some(true));
        assert!(progress.plan.is_none() && progress.nodes.is_empty());
    }
}

#[tokio::test]
async fn unrelated_profile_nodes_are_not_attached_to_a_different_selected_plan() {
    let mut io = FakeRead::new(vec![
        Ok(vec![layout(true, true)]),
        Ok(vec![request()]),
        Ok(vec![profile(0), profile(1), guard(&[])]),
    ]);
    let progress = observe(&mut io).await.unwrap();
    assert!(progress.plan.is_some());
    assert!(progress.nodes.is_empty());
}

#[tokio::test]
async fn cancellation_query_limits_and_native_failures_issue_no_fallback_read() {
    let failures: Vec<ReadResult<Vec<Rec>>> = vec![
        Err(ReadError::Stopped(CollectionStop::Cancelled)),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
        Err(ReadError::Stopped(CollectionStop::QueryLimit)),
        Err(ReadError::Source(invalid_data())),
    ];
    for failure in failures {
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), failure]);
        assert!(observe(&mut io).await.is_err());
        assert_eq!(io.calls.len(), 3);
    }
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(observe(&mut io).await.is_err());
    assert_eq!(io.calls.len(), 2);
}

struct Reconnected {
    inner: FakeRead,
    generation: u64,
    change_at: usize,
}
#[async_trait::async_trait]
impl NativeRead for Reconnected {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        let result = self.inner.query(sql, parameters, limit).await;
        if self.inner.calls.len() == self.change_at {
            self.generation += 1;
        }
        result
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

#[tokio::test]
async fn a_replaced_native_reader_is_not_a_request_or_compute_generation() {
    for change_at in [0, 2, 3] {
        let inner =
            FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), Ok(vec![profile(0), guard(&[])])]);
        let mut reader = Reconnected { inner, generation: 1, change_at };
        let result = read(&mut reader, 42).await;
        if change_at == 0 {
            let progress = present("42", result.unwrap()).unwrap();
            assert!(progress.plan.is_some());
            assert!(progress.request_key.is_none());
        } else {
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::SourceChanged);
            assert_eq!(reader.inner.calls.len(), change_at);
        }
    }
}

#[test]
fn strict_native_plan_entry_keeps_unique_unhashed_payloads_but_refuses_missing_or_duplicate_hashes() {
    assert!(plan_xml::parse_native_statement(XML, Some("0x0123456789ABCDEF")).is_ok());
    assert!(plan_xml::parse_native_statement(XML, Some("0x9999999999999999")).is_err());
    let without_hash = XML.replace(" QueryHash=\"0x0123456789ABCDEF\"", "");
    assert!(plan_xml::parse_native_statement(&without_hash, None).is_ok());
    assert!(plan_xml::parse_native_statement(&without_hash, Some("0x0123456789ABCDEF")).is_err());
    let statement = XML.split("<Statements>").nth(1).unwrap().split("</Statements>").next().unwrap();
    let multiple = XML.replace("</Statements>", &format!("{statement}</Statements>"));
    assert!(plan_xml::parse_native_statement(&multiple, Some("0x0123456789ABCDEF")).is_err());
    assert!(plan_xml::parse_native_statement(&multiple, None).is_err());
}
