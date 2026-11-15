use super::{
    tests::{compilation_identities, database_compilation_row, query_without_costs, unpartitioned_participants},
    *,
};
use crate::{
    query_store::tests::compilation_row,
    read::{NativeRead, ReadError},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{
    QueryCosts, SourceState,
    signals::CompileHistory,
    telemetry::{CollectionStop, SourceShutdown},
};

#[tokio::test]
async fn original_agreeing_population_definitions_supply_policy_only_after_quiet_native_close_without_extra_reads() {
    use crate::query_store::costs::CompilationStatement;
    use culprit_core::telemetry::{DefinitionPayload, PayloadUnavailable, StatementOrigin};
    let identities = compilation_identities();
    let original = &identities["3"];
    let statement = CompilationStatement {
        identity: original.clone(),
        origin: StatementOrigin::Application,
        statement: DefinitionPayload::sql("SELECT id FROM dbo.orders OPTION (RECOMPILE)", culprit_core::Engine::Mssql),
    };
    for outcome in ["agree", "missing", "conflict", "wrong-identity", "unconfirmed"] {
        let mut changed = statement.clone();
        if outcome == "conflict" {
            changed.statement =
                DefinitionPayload::sql("SELECT id FROM dbo.changed OPTION (RECOMPILE)", culprit_core::Engine::Mssql);
        }
        if outcome == "wrong-identity" {
            changed.identity.qualifiers.insert("context_settings_id".into(), "99".into());
        }
        let mut queries = [query_without_costs(), query_without_costs()];
        let mut participants = unpartitioned_participants(&mut queries, &identities);
        participants[0].compilation_statement = Some(&statement);
        participants[1].compilation_statement = if outcome == "missing" { None } else { Some(&changed) };
        let mut io =
            FakeRead::new(vec![Ok(vec![compilation_row()]), Ok(vec![database_compilation_row()])]).with_query_limit(2);
        if outcome == "unconfirmed" {
            io.shutdown = SourceShutdown::Unconfirmed;
        }
        let mut coverage = Coverage::default();
        let read = collect_query_compilations(Reader::new(&mut io), 30, &mut participants, &mut coverage).await;
        drop(participants);
        assert_eq!(io.calls.len(), 2, "{outcome}");
        assert_eq!(io.closed, 1);
        assert!(!io.calls[0].sql.contains("query_store_query_text"));
        if outcome == "unconfirmed" {
            assert!(read.snapshot.is_none());
            assert!(queries.iter().all(|query| query.costs.is_none()));
            continue;
        }
        let snapshot = read.snapshot.unwrap();
        let culprit_core::telemetry::OperationalFact::MssqlQueryCompilation(native) = &snapshot.observations[0].fact
        else {
            panic!()
        };
        assert_eq!(native.compilations, "9007199254740993");
        assert_eq!(native.statement.is_available(), outcome == "agree");
        if outcome == "missing" {
            assert_eq!(native.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
            assert_eq!(
                coverage.outcomes()[0].state,
                SourceState::Collected,
                "missing optional SQL does not invalidate counter coverage"
            );
        }
        if matches!(outcome, "conflict" | "wrong-identity") {
            assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
        }
    }
}

#[tokio::test]
async fn matching_native_identity_retains_compile_history_without_borrowing_runtime_lifetime() {
    let row = replace(
        &compilation_row(),
        &[
            ("count_compiles", Val::Int(5)),
            ("compiling_s", Val::Int(7200)),
            ("query_initial_compile_at", Val::Text("2020-01-01T08:00:00.153333300+02:00".into())),
        ],
    );
    let mut queries = vec![query_without_costs()];
    let original_sql = queries[0].text.clone();
    let mut coverage = Coverage::default();
    let signals = collect_query_compilations(
        Reader::new(FakeRead::new(vec![Ok(vec![row]), Ok(vec![database_compilation_row()])])),
        30,
        &mut unpartitioned_participants(&mut queries, &compilation_identities()),
        &mut coverage,
    )
    .await;
    let costs = queries[0].costs.as_ref().unwrap();
    let history = costs.compile_history.as_ref().unwrap();
    assert_eq!(history.count, 5.0);
    assert!(!history.within_window);
    assert!(costs.plan_ms.is_none() && costs.plans.is_none());
    assert_eq!(history.memory.unwrap().last_kb, Some(8192.0));
    assert_eq!(queries[0].text, original_sql);
    assert_eq!(signals.signals.len(), 2);
    let captured = signals.snapshot.unwrap();
    assert!(captured.is_valid());
    let [row] = captured.observations.as_slice() else { panic!("one native compilation") };
    let culprit_core::telemetry::OperationalFact::MssqlQueryCompilation(native) = &row.fact else { panic!() };
    assert_eq!(native.identity, compilation_identities()["3"]);
    assert_eq!(native.compilations, "5");
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
}

#[tokio::test]
async fn recycled_native_query_qualifiers_cannot_attach_compilation_to_earlier_sql() {
    for (field, changed) in [
        ("context_settings_id", Val::Int(6)),
        ("object_id", Val::Int(123)),
        ("query_hash", Val::Text("FEDCBA9876543210".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
    ] {
        let changed = replace(&compilation_row(), &[(field, changed)]);
        let mut queries = vec![query_with_previous_compilation()];
        let original_sql = queries[0].text.clone();
        let mut coverage = Coverage::default();
        let signals = collect_query_compilations(
            Reader::new(FakeRead::new(vec![Ok(vec![changed]), Ok(vec![database_compilation_row()])])),
            60,
            &mut unpartitioned_participants(&mut queries, &compilation_identities()),
            &mut coverage,
        )
        .await;
        assert_compilation_withheld(&queries[0]);
        assert_eq!(queries[0].text, original_sql);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Partial, "{field}");
        assert_eq!(signals.signals.len(), 2, "independent database totals remain their own measured context");
        assert!(signals.snapshot.is_some(), "the independent native incarnation stays available without old SQL");
    }
}

#[tokio::test]
async fn absent_definition_or_compilation_stays_unknown_without_retaining_an_old_attachment() {
    for (identities, rows) in [(HashMap::new(), vec![compilation_row()]), (compilation_identities(), vec![])] {
        let mut queries = vec![query_with_previous_compilation()];
        let mut coverage = Coverage::default();
        collect_query_compilations(
            Reader::new(FakeRead::new(vec![Ok(rows), Ok(vec![database_compilation_row()])])),
            60,
            &mut unpartitioned_participants(&mut queries, &identities),
            &mut coverage,
        )
        .await;
        assert_compilation_withheld(&queries[0]);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    }
}

#[tokio::test]
async fn one_unknown_query_does_not_erase_an_independently_matched_compilation() {
    let mut other = query_without_costs();
    other.id = "4".into();
    let mut queries = vec![query_without_costs(), other];
    let mut coverage = Coverage::default();
    collect_query_compilations(
        Reader::new(FakeRead::new(vec![
            Ok(vec![compilation_row(), replace(&compilation_row(), &[("query_id", Val::Int(4))])]),
            Ok(vec![database_compilation_row()]),
        ])),
        60,
        &mut unpartitioned_participants(&mut queries, &compilation_identities()),
        &mut coverage,
    )
    .await;
    assert!(queries[0].costs.as_ref().unwrap().compile_history.is_some());
    assert!(queries[1].costs.is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
}

#[tokio::test]
async fn native_timeout_or_cancellation_prevents_followup_sql_and_clears_unadmitted_compilation() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled] {
        let mut queries = vec![query_with_previous_compilation()];
        let mut coverage = Coverage::default();
        let signals = collect_query_compilations(
            Reader::new(FakeRead::new(vec![Err(ReadError::Stopped(stop))])),
            60,
            &mut unpartitioned_participants(&mut queries, &compilation_identities()),
            &mut coverage,
        )
        .await;
        assert!(signals.signals.is_empty());
        assert!(signals.snapshot.is_none());
        assert_compilation_withheld(&queries[0]);
        assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
    }
}

#[tokio::test]
async fn duplicate_or_unrequested_native_compilation_rows_retire_the_reader_without_more_sql() {
    for rows in
        [vec![compilation_row(), compilation_row()], vec![replace(&compilation_row(), &[("query_id", Val::Int(7))])]]
    {
        let mut reader = Reader::new(FakeRead::new(vec![Ok(rows)]));
        let error = reader.compilations(&["3".into()]).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert!(reader.read_rows("must not execute").await.is_err());
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

fn query_with_previous_compilation() -> culprit_core::QueryStat {
    let mut query = query_without_costs();
    query.costs = Some(QueryCosts {
        failed: Some(1.0),
        plans: Some(5.0),
        plan_ms: Some(10.0),
        compile_history: Some(CompileHistory { count: 5.0, ..Default::default() }),
        ..Default::default()
    });
    query
}

fn assert_compilation_withheld(query: &culprit_core::QueryStat) {
    let costs = query.costs.as_ref().unwrap();
    assert_eq!(costs.failed, Some(1.0), "independent runtime costs remain intact");
    assert!(costs.plan_ms.is_none() && costs.plans.is_none() && costs.compile_history.is_none());
}

// Borrow the existing native fixture so this owner can inspect exact query and closure accounting afterward.
#[async_trait::async_trait]
impl NativeRead for &mut FakeRead {
    async fn query(
        &mut self,
        sql: &str,
        parameters: Vec<crate::read::Parameter>,
        limit: usize,
    ) -> crate::read::ReadResult<Vec<crate::value::Rec>> {
        <FakeRead as NativeRead>::query(self, sql, parameters, limit).await
    }
    fn queries(&self) -> u32 {
        <FakeRead as NativeRead>::queries(self)
    }
    async fn close(&mut self) -> SourceShutdown {
        <FakeRead as NativeRead>::close(self).await
    }
}

#[tokio::test]
async fn replica_participants_share_one_compilation_acquisition_without_borrowing_global_compile_tax() {
    let mut queries = vec![
        query_with_previous_compilation(),
        query_with_previous_compilation(),
        query_with_previous_compilation(),
        query_without_costs(),
    ];
    queries[0].query_hash = Some("0x0123456789ABCDEF".into());
    queries[1].query_hash = queries[0].query_hash.clone();
    queries[2].query_hash = queries[0].query_hash.clone();
    queries[3].query_hash = Some("0xFEDCBA9876543210".into());
    let identities = compilation_identities();
    let mut participants = unpartitioned_participants(&mut queries, &identities);
    participants[0].runtime_scope = QueryStoreScope::ReplicaGroup { id: "7".into() };
    participants[1].runtime_scope = QueryStoreScope::ReplicaGroup { id: "8".into() };
    participants[2].runtime_scope = QueryStoreScope::Unknown;
    participants[3].kind = Some(WorkloadKind::PlanCache);
    participants[3].identity = None;
    participants[3].runtime_scope = QueryStoreScope::Unknown;
    assert_eq!(participant_hashes(&participants), ["0x0123456789ABCDEF", "0xFEDCBA9876543210"]);
    let mut io = FakeRead::new(vec![
        Ok(vec![replace(&compilation_row(), &[("compiling_s", Val::Int(60))])]),
        Ok(vec![database_compilation_row()]),
    ])
    .with_query_limit(2);
    let mut coverage = Coverage::default();
    let captured = collect_query_compilations(Reader::new(&mut io), 60, &mut participants, &mut coverage).await;
    drop(participants);
    for query in &queries[..3] {
        let costs = query.costs.as_ref().unwrap();
        assert!(
            costs.compile_history.as_ref().unwrap().within_window,
            "time bounds alone are not population qualification"
        );
        assert!(costs.plan_ms.is_none() && costs.plans.is_none());
        assert_eq!(costs.failed, Some(1.0));
    }
    assert!(
        queries[3].costs.is_none(),
        "Query Store compilation cannot attach to a cache participant sharing its numeric ID"
    );
    assert_eq!(captured.snapshot.unwrap().observations.len(), 1, "one raw native history is retained once");
    assert_eq!(captured.signals.len(), 2, "database totals remain one independent native context");
    assert_eq!(io.calls.len(), 2);
    assert_eq!(io.closed, 1);
    let crate::read::Parameter::Text(ids) = &io.calls[0].parameters[0] else { panic!("native query IDs") };
    assert_eq!(ids, "<ids><id id=\"3\"/></ids>", "duplicate population references do not expand native selection");
    assert_eq!(coverage.outcomes()[0].state, SourceState::Collected);
}

#[tokio::test]
async fn same_query_id_with_different_native_incarnation_is_withheld_only_for_that_participant() {
    let identities = compilation_identities();
    let original = &identities["3"];
    let mut changed = original.clone();
    *changed.qualifiers.get_mut("context_settings_id").unwrap() = "99".into();
    let mut queries = [query_with_previous_compilation(), query_with_previous_compilation()];
    let mut iterator = queries.iter_mut();
    let mut participants = vec![
        QueryParticipant {
            query: iterator.next().unwrap(),
            identity: Some(original),
            compilation_statement: None,
            kind: Some(WorkloadKind::QueryStore),
            runtime_scope: QueryStoreScope::Local,
        },
        QueryParticipant {
            query: iterator.next().unwrap(),
            identity: Some(&changed),
            compilation_statement: None,
            kind: Some(WorkloadKind::QueryStore),
            runtime_scope: QueryStoreScope::Local,
        },
    ];
    let mut io = FakeRead::new(vec![Ok(vec![compilation_row()]), Ok(vec![database_compilation_row()])]);
    let mut coverage = Coverage::default();
    let captured = collect_query_compilations(Reader::new(&mut io), 60, &mut participants, &mut coverage).await;
    drop(participants);
    assert!(queries[0].costs.as_ref().unwrap().compile_history.is_some());
    assert_compilation_withheld(&queries[1]);
    assert!(captured.snapshot.is_some());
    assert_eq!(captured.signals.len(), 2);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
    assert_eq!(io.calls.len(), 2);
}

#[tokio::test]
async fn unconfirmed_compilation_owner_clears_every_selected_population_without_fresh_shared_totals() {
    let identities = compilation_identities();
    let mut queries = vec![query_with_previous_compilation(), query_with_previous_compilation()];
    let mut participants = unpartitioned_participants(&mut queries, &identities);
    for participant in &mut participants {
        participant.runtime_scope = QueryStoreScope::Unknown;
    }
    let mut io = FakeRead::new(vec![Ok(vec![compilation_row()]), Ok(vec![database_compilation_row()])]);
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut coverage = Coverage::default();
    let captured = collect_query_compilations(Reader::new(&mut io), 60, &mut participants, &mut coverage).await;
    drop(participants);
    for query in &queries {
        assert_compilation_withheld(query);
    }
    assert!(captured.snapshot.is_none() && captured.signals.is_empty());
    assert_eq!(io.closed, 1);
    assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
}

#[tokio::test]
async fn failed_query_compilation_keeps_only_independent_database_context_and_explicit_partial_source() {
    let identities = compilation_identities();
    let mut queries = vec![query_with_previous_compilation(), query_with_previous_compilation()];
    let mut participants = unpartitioned_participants(&mut queries, &identities);
    let mut io = FakeRead::new(vec![
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture source denied",
        ))),
        Ok(vec![database_compilation_row()]),
    ]);
    let mut coverage = Coverage::default();
    let captured = collect_query_compilations(Reader::new(&mut io), 60, &mut participants, &mut coverage).await;
    drop(participants);
    for query in &queries {
        assert_compilation_withheld(query);
    }
    assert!(captured.snapshot.is_none());
    assert_eq!(captured.signals.len(), 2);
    let outcome = &coverage.outcomes()[0];
    assert_eq!(outcome.state, SourceState::Partial);
    assert!(outcome.reason.is_some(), "successful independent context cannot erase the source's failure reason");
    assert_eq!(io.calls.len(), 2);
    assert_eq!(io.closed, 1);
}
