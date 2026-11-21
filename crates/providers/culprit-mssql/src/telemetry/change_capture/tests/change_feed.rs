use super::*;
fn selected() -> CollectionRequest {
    let mut r = request();
    r.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::ChangeFeed };
    r
}
fn native_replies() -> Vec<ReadResult<Vec<Rec>>> {
    let mut rows = replies();
    rows[3] = Ok(vec![crate::change_feed::tests::availability_row()]);
    rows[5] = Ok(vec![crate::change_feed::tests::scan_row()]);
    rows[7] = Ok(vec![crate::change_feed::tests::error_row()]);
    rows
}
#[tokio::test]
async fn change_feed_reuses_original_lifecycle_twelve_reads_plus_one_reserved_setup_and_distinct_sources() {
    let (mut source, trace) = source_with_replies(native_replies(), &selected(), Some(1));
    let description = source.describe().await.unwrap();
    let rows = drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 12);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(description.observation.snapshot().unwrap().dataset, SnapshotDataset::ChangeFeed);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row|matches!(row,TelemetryRecord::Operational(row) if matches!(&row.fact,OperationalFact::MssqlChangeFeed(point) if point.fits_source(&description)))));
    assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
    let calls = trace.calls.lock().unwrap();
    assert!(!calls.iter().any(|(sql, _)| sql.contains("sys.dm_cdc_")));
    assert_eq!(bounded_budget(&CollectionBudget::upper_limits()).max_queries, 13);
}
#[tokio::test]
async fn either_native_permission_admits_and_unknown_denied_or_missing_layout_does_not_claim_empty() {
    for (state, performance, expected, skip_scans) in [
        (Val::Int(1), Val::Null, SourceState::Collected, false),
        (Val::Null, Val::Int(1), SourceState::Collected, false),
        (Val::Int(0), Val::Int(0), SourceState::Unavailable, true),
        (Val::Null, Val::Null, SourceState::Unavailable, true),
    ] {
        let mut rows = native_replies();
        rows[3] = Ok(vec![replace(
            &crate::change_feed::tests::availability_row(),
            &[("has_database_state", state), ("has_database_performance_state", performance)],
        )]);
        if skip_scans {
            rows.drain(4..8);
        }
        let (mut source, _) = source_with_replies(rows, &selected(), Some(1));
        let d = source.describe().await.unwrap();
        drain(&mut source).await;
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        source.close().await;
        let h = d.observation.snapshot().unwrap();
        assert_eq!(h.sources[1].state, expected);
        assert_eq!(h.sources[2].state, expected);
    }
    let mut rows = native_replies();
    rows[3] = Ok(vec![replace(&crate::change_feed::tests::availability_row(), &[("scan_layout", Val::Int(0))])]);
    rows.drain(4..6);
    let (mut source, _) = source_with_replies(rows, &selected(), Some(1));
    let d = source.describe().await.unwrap();
    drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    source.close().await;
    let h = d.observation.snapshot().unwrap();
    assert_eq!(h.sources[1].state, SourceState::Unavailable);
    assert_eq!(h.sources[2].records, 1);
}
#[tokio::test]
async fn independent_completed_scan_error_partial_tail_and_native_owner_or_clock_changes_remain_distinct() {
    let mut rows = native_replies();
    rows[5] =
        Err(crate::read::ReadError::Source(Error::new(culprit_core::ErrorCode::QueryFailed, "private native text")));
    let (mut source, _) = source_with_replies(rows, &selected(), Some(1));
    let d = source.describe().await.unwrap();
    let records = drain(&mut source).await;
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    source.close().await;
    assert_eq!(d.observation.snapshot().unwrap().sources[1].state, SourceState::Unavailable);
    assert_eq!(records.len(), 2);
    let mut rows = native_replies();
    rows[7] = Ok(vec![crate::change_feed::tests::error_row(); MAX_MSSQL_CHANGE_FEED_ERROR_ROWS + 1]);
    let (mut source, _) = source_with_replies(rows, &selected(), Some(1));
    let d = source.describe().await.unwrap();
    let retained = drain(&mut source).await;
    assert_eq!(d.observation.snapshot().unwrap().sources[2].state, SourceState::Partial);
    assert_eq!(d.observation.snapshot().unwrap().sources[2].records, 64);
    assert_eq!(retained.len(), 66);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    source.close().await;
    for change in ["epoch", "role", "clock", "dbid"] {
        let mut rows = native_replies();
        if change == "epoch" {
            rows[10] = Ok(vec![crate::instance::test_support::epoch(101)]);
        } else {
            let row = rows[11].as_ref().unwrap()[0].clone();
            rows[11] = Ok(vec![replace(
                &row,
                &[match change {
                    "role" => ("updateability", Val::Text("READ_ONLY".into())),
                    "clock" => ("observed_at", Val::Text("2020-01-01T06:00:00.500Z".into())),
                    _ => ("selected_database_id", Val::Int(6)),
                }],
            )]);
        }
        let (mut source, _) = source_with_replies(rows, &selected(), Some(1));
        source.describe().await.unwrap();
        drain(&mut source).await;
        assert!(!matches!(source.finish().await, Ok(SourceCompletion::Verified)), "{change}");
        source.close().await;
    }
}
#[tokio::test]
async fn source_caller_total_reserves_setup_and_cannot_fund_the_final_native_guard() {
    let mut request = selected();
    request.budget.max_queries = 12;
    let budget = ReadSession::budget_after_setup(bounded_budget(&request.budget)).unwrap();
    assert_eq!(budget.max_queries, 11);
    let trace = Arc::new(Trace::default());
    let native = FakeRead::new(native_replies()).with_query_limit(budget.max_queries);
    let mut source = ChangeCaptureSource::with_io(
        Reader { native, trace: trace.clone(), generation: Some(1) },
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    source.describe().await.unwrap();
    drain(&mut source).await;
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 11);
    source.close().await;
    request.budget.max_queries = 1;
    assert!(ReadSession::budget_after_setup(bounded_budget(&request.budget)).is_err());
}
