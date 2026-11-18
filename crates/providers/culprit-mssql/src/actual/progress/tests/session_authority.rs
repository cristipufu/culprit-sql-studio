use super::*;
use crate::activity::ESTABLISHED_SESSION_SQL;

#[tokio::test]
async fn a_known_unestablished_request_stops_before_statement_enrichment_or_profiles() {
    // The fixed native branch supplies only its clock/count/refusal; no selector or SQL payload is needed.
    let row = Rec::of(&[
        ("request_count", Val::Int(1)),
        ("session_established", Val::Bool(false)),
        ("observed_at", Val::Text("2020-01-01T12:00:00Z".into())),
    ]);
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row])]);
    let progress = observe(&mut io).await.unwrap();
    assert_eq!(progress.running, None);
    assert!(progress.statement.is_none() && progress.plan.is_none() && progress.nodes.is_empty());
    assert_eq!(io.calls.len(), 2);
    let sql = &io.calls[1].sql;
    assert!(sql.contains(ESTABLISHED_SESSION_SQL));
    let refused = sql.find("IF NOT EXISTS(SELECT 1 FROM @selected WHERE session_established=1)").unwrap();
    for enrichment in ["sys.dm_exec_sql_text", "sys.dm_exec_query_statistics_xml", "sys.dm_exec_text_query_plan"] {
        assert!(refused < sql.find(enrichment).unwrap(), "{enrichment}");
    }
}

#[tokio::test]
async fn a_parseable_prelogin_sentinel_cannot_borrow_an_inconsistent_positive_native_flag() {
    let row = replace(&request(), &[("checked_session_generation", Val::Text("1900-01-01T00:00:00.0000000".into()))]);
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row])]);
    let progress = observe(&mut io).await.unwrap();
    assert_eq!(progress.running, None);
    assert!(progress.statement.is_none() && progress.plan.is_none() && progress.nodes.is_empty());
    assert_eq!(io.calls.len(), 2);
}

#[tokio::test]
async fn missing_or_null_establishment_fails_closed_without_a_rescue_or_profile_query() {
    for with_null in [false, true] {
        let row = request();
        let fields = row
            .columns()
            .filter(|(name, _)| *name != "session_established")
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>();
        let mut row = Rec::of(&fields);
        if with_null {
            row = replace(&request(), &[("session_established", Val::Null)]);
        }
        let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row])]);
        assert!(observe(&mut io).await.is_err());
        assert_eq!(io.calls.len(), 2);
    }
}

#[tokio::test]
async fn an_unestablished_final_guard_discards_already_read_profiles_and_keeps_the_last_clock() {
    let guard = guard(&[("session_established", Val::Bool(false))]);
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![request()]), Ok(vec![profile(0), guard])]);
    let progress = observe(&mut io).await.unwrap();
    assert_eq!(progress.running, None);
    assert!(progress.plan.is_none() && progress.nodes.is_empty() && progress.statement.is_none());
    assert_eq!(progress.taken_at, "2020-01-01T12:00:00.500000000Z");
    assert_eq!(io.calls.len(), 3);
    assert!(
        io.calls[2].sql.matches(ESTABLISHED_SESSION_SQL).count() == 2,
        "profiles and final guard use the same policy"
    );
}

#[tokio::test]
async fn native_mars_cardinality_is_not_filtered_into_one_established_request() {
    let row = Rec::of(&[
        ("request_count", Val::Int(2)),
        ("session_established", Val::Bool(false)),
        ("observed_at", Val::Text("2020-01-01T12:00:00Z".into())),
    ]);
    let mut io = FakeRead::new(vec![Ok(vec![layout(true, true)]), Ok(vec![row])]);
    let progress = observe(&mut io).await.unwrap();
    assert_eq!(progress.running, None);
    assert_eq!(progress.note, Some(t!("provider.mssql.progress.ambiguous_request")));
    assert_eq!(io.calls.len(), 2);
    let sql = &io.calls[1].sql;
    assert!(
        sql.find("IF (SELECT COUNT(*) FROM @selected)<>1").unwrap() < sql.find("WHERE session_established=1").unwrap()
    );
}
