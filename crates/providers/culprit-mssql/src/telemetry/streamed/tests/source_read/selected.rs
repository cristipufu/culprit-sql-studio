//! The actual streamed owner consumes controlled protocol rows; no durable parent/native cost is qualified.
use super::*;

struct OriginalQuery {
    source: CollectionDescription,
    row: ExportedRecord,
    request: CollectionRequest,
}
fn original_query(query_id: &str) -> OriginalQuery {
    let mut request = request();
    let CollectionSelection::SourceRead { focus, execution } = &mut request.selection else { panic!() };
    let mut query =
        crate::query_store::costs::Compilation::decode(&crate::query_store::tests::compilation_row()).unwrap().identity;
    query.query_id = query_id.into();
    let plan = NativePlanIdentity {
        plan_id: "7".into(),
        query: query.clone(),
        qualifiers: [("initial_compile_at".into(), query.qualifiers["initial_compile_at"].clone())].into(),
    };
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: SourcePartition::Target,
        source: culprit_core::source_id::QUERY_STORE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval: TelemetryInterval {
                native_id: "11".into(),
                from: "2020-01-01T07:00:00Z".into(),
                until: "2020-01-01T08:00:00Z".into(),
            },
            acquisition_owner: Some(Box::new(NativeReadContext {
                target: focus.target.clone(),
                partition: execution.partition().clone(),
                database_writable: Some(execution.database_writable()),
                observed_from: "2020-01-01T08:00:00Z".into(),
                observed_until: FIRST.into(),
            })),
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: Default::default(),
        }),
    };
    let row = ExportedRecord::new(
        &source,
        &CaptureIdentity {
            observation_id: "selected-query-original".into(),
            revision: Some(CaptureRevision {
                outbox_id: "protocol-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey {
                native: NativeIdentity::new("101").qualified("plan_id", "7").qualified("execution_type", "4"),
                references: StatementReferences {
                    query: Some(NativeDefinitionReference {
                        native: query.native(),
                        definition_id: Some(query.definition_id(&source).unwrap()),
                    }),
                    plan: Some(NativeDefinitionReference {
                        native: plan.native(),
                        definition_id: Some(plan.definition_id(&source).unwrap()),
                    }),
                },
                cohort: Some(NativeIdentity::new("query-store-execution-population").qualified("execution_type", "4")),
            },
            executions: "1".into(),
            metrics: vec![],
            page_server_reads: None,
            first_execution_at: None,
            last_execution_at: None,
        }),
    )
    .unwrap();
    focus.basis = Box::new(
        DiagnosticEvidenceRef::from_export(&source, &digest(b"selected-query-original-manifest"), &row, 0).unwrap(),
    );
    focus.prerequisite = SourceReadPrerequisite::ObservedQueryHistoryFailure { query };
    request.validate().unwrap();
    OriginalQuery { source, row, request }
}
fn selected_source(
    request: &CollectionRequest,
    batches: Vec<ReadResult<ReadBatch>>,
    shutdown: SourceShutdown,
) -> StreamedSnapshot<QueryCompilation, StreamingRead> {
    let mut io = native(authority(FIRST), authority(LAST), shutdown);
    io.batches = batches.into();
    StreamedSnapshot::with_io(io, request, "fixture.invalid".into(), 1433, QueryCompilation).unwrap()
}
fn answer(
    original: &OriginalQuery,
    source: &CollectionDescription,
    records: Vec<TelemetryRecord>,
) -> QueryCompilationAnswer {
    let capture = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "selected-query-result".into(),
            revision: original.row.capture.revision.clone(),
        },
        manifest_sha256: digest(b"selected-query-result-manifest"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let exports = records
        .into_iter()
        .map(|record| ExportedRecord::new(source, &capture.capture, record).unwrap())
        .collect::<Vec<_>>();
    let references = exports
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            DiagnosticEvidenceRef::from_export(source, &capture.manifest_sha256, row, ordinal as u32).unwrap()
        })
        .collect::<Vec<_>>();
    let CollectionSelection::SourceRead { focus, .. } = &original.request.selection else { panic!() };
    QueryCompilationAnswer::from_originals(
        focus,
        &original.source,
        &original.row,
        source,
        &capture,
        &references.iter().zip(&exports).collect::<Vec<_>>(),
    )
    .unwrap()
}
fn assert_selected_call(source: &StreamedSnapshot<QueryCompilation, StreamingRead>, id: &str, limit: usize) {
    let calls = &source.inner.reader().io.native.calls;
    assert_eq!(calls.len(), 6, "four description reads, existing pressure admission, one body stream");
    let call = &calls[5];
    assert_eq!(call.sql, crate::query_store::costs::compilation_sql());
    let [Parameter::Text(actual)] = call.parameters.as_slice() else { panic!("one typed native ID selector") };
    let Parameter::Text(expected) = crate::query_store::reader::ids_parameter(&[id.into()]).unwrap() else { panic!() };
    assert_eq!(actual, &expected);
    assert_eq!(call.limit, limit);
}

#[tokio::test]
async fn original_query_reuses_selected_template_encoder_decoder_and_eight_read_owner() {
    let original = original_query("3");
    let mut source = selected_source(
        &original.request,
        vec![batch(crate::query_store::tests::compilation_row(), true)],
        SourceShutdown::Quiescent,
    );
    assert_eq!(source.query_count(), 0);
    let initial = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 4);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert!(next_cursor.is_none());
    assert_selected_call(&source, "3", original.request.budget.max_snapshot_rows as usize + 1);
    let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
    assert_eq!(source.query_count(), 8);
    let answer = answer(&original, &completed, records);
    let QueryCompilationSelection::Observed { observation, counters } = &answer.selection else { panic!() };
    assert_eq!(observation.row_ordinal, 0, "position in this genuinely selected source stream");
    assert_eq!(counters.compilations, "9007199254740993");
    assert_eq!(counters.average_duration_us, 1500.5);
    assert_eq!(counters.reset, culprit_core::ResetMarker::Unknown);
    assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.inner.reader().io.native.closed, 1);
}

#[tokio::test]
async fn general_snapshot_and_compiler_gateway_history_keep_all_query_statement_and_limit() {
    let request = request();
    let mut source = selected_source(
        &request,
        vec![batch(crate::query_store::tests::compilation_row(), true)],
        SourceShutdown::Quiescent,
    );
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    let call = &source.inner.reader().io.native.calls[5];
    assert_eq!(call.sql, crate::query_store::costs::compilation_snapshot_sql());
    assert!(
        matches!(call.parameters.as_slice(), [Parameter::Integer(limit)] if *limit == request.budget.max_snapshot_rows as i64 + 1)
    );
    assert_eq!(call.limit, request.budget.max_snapshot_rows as usize + 1);
    source.finish().await.unwrap();
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut snapshot = catalog_source(
        QueryCompilation,
        crate::query_store::tests::compilation_row(),
        true,
        true,
        request.budget.clone(),
    );
    snapshot.describe().await.unwrap();
    snapshot.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    let call = &snapshot.inner.reader().io.native.calls[5];
    assert_eq!(call.sql, crate::query_store::costs::compilation_snapshot_sql());
    assert!(
        matches!(call.parameters.as_slice(), [Parameter::Integer(limit)] if *limit == request.budget.max_snapshot_rows as i64 + 1)
    );
    snapshot.finish().await.unwrap();
    assert_eq!(snapshot.query_count(), 8);
    assert_eq!(snapshot.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_and_recycled_incarnations_keep_explicit_answer_gaps_without_fallback() {
    for changed in [false, true] {
        let original = original_query("3");
        let rows = if changed {
            vec![replace(
                &crate::query_store::tests::compilation_row(),
                &[("query_initial_compile_at", Val::Text("2020-01-01T06:00:01.1533333Z".into()))],
            )]
        } else {
            vec![]
        };
        let mut source =
            selected_source(&original.request, vec![Ok(ReadBatch { rows, complete: true })], SourceShutdown::Quiescent);
        let initial = source.describe().await.unwrap();
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!()
        };
        assert!(next_cursor.is_none());
        assert_selected_call(&source, "3", original.request.budget.max_snapshot_rows as usize + 1);
        let completed = initial.completed(source.finish().await.unwrap()).unwrap().unwrap();
        let answer = answer(&original, &completed, records);
        assert_eq!(
            answer.selection,
            if changed {
                QueryCompilationSelection::IncarnationChanged
            } else {
                QueryCompilationSelection::NotObserved
            }
        );
        assert_eq!(
            answer.source_state,
            if changed { culprit_core::SourceState::Collected } else { culprit_core::SourceState::Empty }
        );
        assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
        assert_eq!(source.query_count(), 8);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn bigint_and_caller_row_bounds_remain_exact_and_foreign_query_rows_refuse() {
    let original = original_query(&i64::MAX.to_string());
    let row = replace(&crate::query_store::tests::compilation_row(), &[("query_id", Val::Int(i64::MAX))]);
    let mut source = selected_source(&original.request, vec![batch(row, true)], SourceShutdown::Quiescent);
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_selected_call(&source, &i64::MAX.to_string(), original.request.budget.max_snapshot_rows as usize + 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let mut original = original_query("3");
    original.request.budget.max_snapshot_rows = 1;
    original.request.budget.page_rows = 1;
    let rows = vec![crate::query_store::tests::compilation_row(); 2];
    let mut source =
        selected_source(&original.request, vec![Ok(ReadBatch { rows, complete: true })], SourceShutdown::Quiescent);
    source.describe().await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap_err()),
        CollectionStop::RowLimit
    );
    assert_selected_call(&source, "3", 2);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6, "no final or replacement query after overflow");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let row = replace(&crate::query_store::tests::compilation_row(), &[("query_id", Val::Int(4))]);
    let mut source = selected_source(&original.request, vec![batch(row, true)], SourceShutdown::Quiescent);
    source.describe().await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap_err()),
        CollectionStop::SourceChanged
    );
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unrepresentable_native_id_refuses_before_streaming_without_all_query_fallback() {
    let original = original_query("9223372036854775808");
    let mut source = selected_source(&original.request, vec![], SourceShutdown::Quiescent);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.inner.reader().io.starts, 0);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cancellation_retirement_and_final_owner_change_preserve_failure_and_shutdown_without_more_work() {
    let original = original_query("3");
    let mut source = selected_source(
        &original.request,
        vec![Err(ReadError::Stopped(CollectionStop::Cancelled))],
        SourceShutdown::Unconfirmed,
    );
    source.describe().await.unwrap();
    assert_eq!(
        CollectionStop::from_error(&source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap_err()),
        CollectionStop::Cancelled
    );
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    let mut source = selected_source(&original.request, vec![], SourceShutdown::Unconfirmed);
    source.describe().await.unwrap();
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.inner.reader().io.starts, 0);
    let after = replace(
        &authority(LAST),
        &[("physical_incarnation", Val::Text("77777777-1111-2222-3333-444444444444".into()))],
    );
    let mut source = StreamedSnapshot::with_io(
        native(authority(FIRST), after, SourceShutdown::Unconfirmed),
        &original.request,
        "fixture.invalid".into(),
        1433,
        QueryCompilation,
    )
    .unwrap();
    source.describe().await.unwrap();
    source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.query_count(), 8);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}
