use super::*;
use std::time::Duration;

const INVOCATION: &str = "0123456789abcdef0123456789abcdef";
fn observation() -> ProviderReadObservation {
    ProviderReadObservation::begin(INVOCATION.into(), ProviderReadKind::Probe).unwrap()
}
fn metrics(completed: u32) -> ReadWorkMetrics {
    let recorder = ReadMetricsRecorder::new(Engine::Mssql);
    for _ in 0..completed {
        let id = recorder.begin("SELECT N'private-sql-canary-9264'");
        recorder.finish(id.as_deref(), Duration::from_millis(1), &ReadProgress::default(), &ReadTaskOutcome::default());
    }
    recorder.snapshot().unwrap()
}
fn gaps(summary: &ProviderAcquisitionSummary) -> &[ProviderReadGap] {
    let ProviderReadCoverage::Partial { gaps } = &summary.coverage else {
        panic!("expected explicitly partial accounting")
    };
    gaps
}

#[test]
fn pre_native_factory_failure_preserves_original_error_and_does_not_borrow_database_identity() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    operation.bind_route(SourceAdmissionRoute::UnsavedConnectionTest { invocation_id: INVOCATION.into() });
    operation.bind_engine(Engine::Mssql);
    let failure = Error::new(ErrorCode::AuthFailed, "private-error-canary-9264")
        .with_hint("private-hint-canary")
        .with_detail(crate::ErrorDetail::ConnectionSecurity);
    let expected = serde_json::to_value(&failure).unwrap();
    let result: Result<()> = Err(failure);
    operation.factory().finish(&result);
    let attempt = owner.finish(result);
    assert_eq!(
        serde_json::to_value(attempt.original().expect("materialized original result").as_ref().unwrap_err()).unwrap(),
        expected
    );
    assert!(attempt.finalization().is_ok());
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.native_disposition, ProviderReadDisposition::NotStarted);
    assert_eq!(summary.original_failure, Some(ProviderReadFailure { code: ErrorCode::AuthFailed, stop: None }));
    assert_eq!(
        summary.factory.as_ref().unwrap().outcome,
        ProviderFactoryOutcome::Rejected { code: ErrorCode::AuthFailed }
    );
    assert_eq!(summary.coverage, ProviderReadCoverage::Unavailable { reason: ProviderReadGap::NotInstrumented });
    let public = serde_json::to_string(summary).unwrap();
    assert!(!public.contains("private-error-canary"));
    assert!(!public.contains("private-hint-canary"));
    assert!(!public.contains("database_identity"));
    assert_eq!(summary.route.as_ref().unwrap().saved_connection(), None);
}

#[test]
fn native_settlement_and_original_result_remain_independent_with_existing_error_precedence() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    operation.admitted();
    operation.native_started();
    let original = CollectionStop::ByteLimit.error();
    let detail = original.detail;
    let _ = operation.source_finished(
        &Err::<(), _>(original.clone()),
        ProviderReadDisposition::Unconfirmed,
        Err(CollectionStop::CancellationUnconfirmed.error()),
    );
    let attempt = owner.finish::<()>(Err(original));
    assert_eq!(attempt.original().expect("materialized original result").as_ref().unwrap_err().detail, detail);
    assert_eq!(attempt.acquisition().original_failure.as_ref().unwrap().stop, Some(CollectionStop::ByteLimit));
    assert_eq!(
        attempt.acquisition().finalization_failure.as_ref().unwrap().stop,
        Some(CollectionStop::CancellationUnconfirmed)
    );
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Unconfirmed);
    assert_eq!(
        CollectionStop::from_error(&attempt.into_result().unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
}

#[test]
fn canceling_an_admitted_future_is_unconfirmed_even_when_all_observed_counters_are_zero() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    operation.admitted();
    let reader = context.reader(Engine::Postgres);
    reader.checkpoint(0, Some(metrics(0)));
    drop(reader);
    let attempt = owner.finish::<()>(Err(CollectionStop::Cancelled.error()));
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Unconfirmed);
    assert_eq!(attempt.acquisition().readers[0].queries, Some(0));
    assert_eq!(attempt.acquisition().readers[0].disposition, ProviderReaderDisposition::Abandoned);
    assert!(gaps(attempt.acquisition()).contains(&ProviderReadGap::ReaderAbandoned));
    assert_eq!(
        CollectionStop::from_error(&attempt.into_result().unwrap_err()),
        CollectionStop::CancellationUnconfirmed
    );
}

#[test]
fn cloned_worker_observers_publish_one_original_absolute_count_without_summing_snapshots() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    let reader = context.reader(Engine::Mssql);
    let first = reader.observer();
    let second = first.clone();
    first.checkpoint(1, Some(metrics(1)));
    second.checkpoint(2, Some(metrics(2)));
    first.checkpoint(2, Some(metrics(2)));
    reader.setup_checkpoint(1, Some(metrics(1)));
    reader.finish(SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(42));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.coverage, ProviderReadCoverage::Complete);
    assert_eq!(summary.readers.len(), 1);
    assert_eq!(summary.readers[0].queries, Some(2));
    assert_eq!(summary.readers[0].setup_queries, Some(1));
    assert_eq!(summary.readers[0].work.as_ref().unwrap().templates[0].completed_tasks, 2);
    assert_eq!(attempt.into_result().unwrap(), 42);
}

#[test]
fn regressing_counts_or_template_snapshots_withdraw_accounting_and_cannot_be_healed_by_close() {
    for lower_count in [true, false] {
        let owner = observation();
        let operation = owner.operation().unwrap();
        let context = operation.context();
        context.complete_reader_coverage();
        let reader = context.reader(Engine::Mssql);
        reader.checkpoint(2, Some(metrics(2)));
        reader.checkpoint(if lower_count { 1 } else { 2 }, Some(metrics(1)));
        reader.checkpoint(3, Some(metrics(3)));
        reader.setup_checkpoint(0, Some(metrics(0)));
        reader.finish(SourceShutdown::Quiescent);
        let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
        let attempt = owner.finish(Ok(()));
        assert!(attempt.acquisition().is_valid());
        let reader = &attempt.acquisition().readers[0];
        assert_eq!(reader.disposition, ProviderReaderDisposition::Unconfirmed);
        assert_eq!((reader.queries, reader.setup_queries), (None, None));
        assert!(reader.work.is_none() && reader.setup_work.is_none());
        assert!(gaps(attempt.acquisition()).contains(&ProviderReadGap::InvalidObservation));
        // Measurement integrity does not independently clear or poison the real gate.
        assert!(attempt.into_result().is_ok());
    }
}

#[test]
fn late_worker_updates_cannot_mutate_a_finished_reader_or_frozen_invocation() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    let reader = context.reader(Engine::Mysql);
    let worker = reader.observer();
    reader.checkpoint(1, Some(metrics(1)));
    reader.setup_checkpoint(0, Some(metrics(0)));
    reader.finish(SourceShutdown::Quiescent);
    worker.checkpoint(2, Some(metrics(2)));
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(()));
    let before = attempt.acquisition().clone();
    worker.checkpoint(3, Some(metrics(3)));
    context.partial_reader_coverage(ProviderReadGap::UnhookedEnrichment);
    assert_eq!(attempt.acquisition(), &before);
    assert_eq!(before.readers[0].queries, Some(1));
}

#[test]
fn reader_cap_preserves_retained_owners_and_makes_omitted_coverage_explicit() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    for _ in 0..MAX_PROVIDER_READERS + 2 {
        let reader = context.reader(Engine::Mssql);
        reader.checkpoint(1, Some(metrics(1)));
        reader.setup_checkpoint(0, Some(metrics(0)));
        reader.finish(SourceShutdown::Quiescent);
    }
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(()));
    assert!(attempt.acquisition().is_valid());
    assert_eq!(attempt.acquisition().readers.len(), MAX_PROVIDER_READERS);
    assert_eq!(attempt.acquisition().omitted_readers, 2);
    assert!(gaps(attempt.acquisition()).contains(&ProviderReadGap::OmittedReaders));
}

#[test]
fn sql_previews_and_error_text_never_enter_curated_current_acquisition_metadata() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    let reader = context.reader(Engine::Mssql);
    reader.checkpoint(1, Some(metrics(1)));
    reader.setup_checkpoint(0, Some(metrics(0)));
    reader.finish(SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish::<()>(Err(Error::new(ErrorCode::QueryFailed, "private-error-canary-9264")));
    let raw = serde_json::to_value(attempt.acquisition()).unwrap();
    let text = raw.to_string();
    assert!(!text.contains("private-sql-canary"));
    assert!(!text.contains("private-error-canary"));
    for forbidden in ["source", "manifest_sha256", "database_identity", "observation_proof", "native_scope"] {
        assert!(!raw.as_object().unwrap().contains_key(forbidden));
    }
    let summary = attempt.acquisition();
    assert!(matches!(
        summary.readers[0].work.as_ref().unwrap().templates[0].statement,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    ));
    let decoded: ProviderAcquisitionSummary = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(&decoded, summary);
    assert!(decoded.is_valid());
    for field in ["route", "engine", "factory", "original_failure", "finalization_failure"] {
        let mut missing = raw.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ProviderAcquisitionSummary>(missing).is_err(), "{field}");
    }
    let mut tampered = decoded;
    tampered.readers[0].work.as_mut().unwrap().templates[0].statement = metrics(1).templates.remove(0).statement;
    assert!(!tampered.is_valid());
}

#[test]
fn partial_sqlite_direct_statement_coverage_is_not_represented_as_complete_zero_work() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.partial_reader_coverage(ProviderReadGap::PartialReaderCoverage);
    let reader = context.reader(Engine::Sqlite);
    reader.setup_checkpoint(2, Some(metrics(2)));
    reader.finish(SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(()));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.readers[0].queries, None);
    assert_eq!(summary.readers[0].setup_queries, Some(2));
    assert!(gaps(summary).contains(&ProviderReadGap::PartialReaderCoverage));
    assert!(gaps(summary).contains(&ProviderReadGap::MissingReaderSnapshot));
}

#[test]
fn duplicate_factory_guards_do_not_finish_or_replace_the_original_constructor_span() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let original = operation.factory();
    operation.factory().finish(&Ok(()));
    original.finish::<()>(&Err(Error::new(ErrorCode::PermissionDenied, "private TLS file")));
    let attempt = owner.finish::<()>(Err(Error::new(ErrorCode::PermissionDenied, "private TLS file")));
    assert_eq!(
        attempt.acquisition().factory.as_ref().unwrap().outcome,
        ProviderFactoryOutcome::Rejected { code: ErrorCode::PermissionDenied }
    );
    assert!(attempt.acquisition().is_valid());
    assert!(gaps(attempt.acquisition()).contains(&ProviderReadGap::InvalidObservation));
}

#[test]
fn finalization_failure_retains_a_successful_original_value_but_cannot_return_success() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let _ = operation.source_finished(
        &Ok::<(), Error>(()),
        ProviderReadDisposition::Quiescent,
        Err(CollectionStop::StorageFailed.error()),
    );
    let attempt = owner.finish(Ok("original probe server result"));
    assert_eq!(
        attempt.original().expect("materialized original result").as_ref().unwrap(),
        &"original probe server result"
    );
    assert_eq!(attempt.acquisition().original_failure, None);
    assert_eq!(attempt.acquisition().finalization_failure.as_ref().unwrap().stop, Some(CollectionStop::StorageFailed));
    assert_eq!(CollectionStop::from_error(&attempt.into_result().unwrap_err()), CollectionStop::StorageFailed);
}

#[test]
fn inconsistent_routes_and_active_or_untracked_work_never_gain_complete_coverage() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    operation.bind_route(SourceAdmissionRoute::UnsavedConnectionTest {
        invocation_id: "1123456789abcdef0123456789abcdef".into(),
    });
    let reader = context.reader(Engine::Mssql);
    let mut work = metrics(0);
    work.active_tasks = 1;
    work.untracked_tasks = 1;
    reader.checkpoint(1, Some(work));
    reader.setup_checkpoint(0, Some(metrics(0)));
    reader.finish(SourceShutdown::Quiescent);
    let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish(Ok(()));
    assert!(attempt.acquisition().is_valid());
    assert!(attempt.acquisition().route.is_none());
    for gap in [ProviderReadGap::InvalidObservation, ProviderReadGap::ReaderAbandoned, ProviderReadGap::UntrackedTasks]
    {
        assert!(gaps(attempt.acquisition()).contains(&gap));
    }
}

#[test]
fn operation_capability_is_unique_and_late_engine_completion_cannot_replace_retired_uncertainty() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    assert!(owner.operation().is_err());
    operation.admitted();
    let attempt = owner.finish::<()>(Err(CollectionStop::Cancelled.error()));
    let before = attempt.acquisition().clone();
    assert_eq!(before.native_disposition, ProviderReadDisposition::Unconfirmed);
    assert!(operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(())).is_none());
    assert_eq!(attempt.acquisition(), &before);
}

#[test]
fn missing_setup_accounting_is_partial_while_an_explicitly_measured_zero_is_complete() {
    for measured in [false, true] {
        let owner = observation();
        let operation = owner.operation().unwrap();
        let context = operation.context();
        context.complete_reader_coverage();
        let reader = context.reader(Engine::Mssql);
        reader.checkpoint(0, Some(metrics(0)));
        if measured {
            reader.setup_checkpoint(0, Some(metrics(0)));
        }
        reader.finish(SourceShutdown::Quiescent);
        let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
        let attempt = owner.finish(Ok(()));
        assert!(attempt.acquisition().is_valid());
        if measured {
            assert_eq!(attempt.acquisition().coverage, ProviderReadCoverage::Complete);
        } else {
            assert!(gaps(attempt.acquisition()).contains(&ProviderReadGap::MissingSetupSnapshot));
        }
    }
}

#[test]
fn original_connection_ledger_may_finish_pending_acquisition_but_cannot_disappear_or_rewrite_completed_work() {
    for alter in 0..3 {
        let owner = observation();
        let operation = owner.operation().unwrap();
        let context = operation.context();
        context.complete_reader_coverage();
        let reader = context.reader(Engine::Mssql);
        let ledger = ReadConnectionRecorder::default();
        let attempt = ledger.begin();
        let phase = attempt.phase(ReadConnectionStage::Acquire, None);
        let mut before = metrics(0);
        before.connection_work = ledger.snapshot().map(Box::new);
        reader.checkpoint(0, Some(before));
        attempt.connection_use(ReadConnectionUse::FreshConnection);
        attempt.acquired(7);
        phase.finish(ReadConnectionOutcome::Completed);
        let mut completed = metrics(0);
        completed.connection_work = ledger.snapshot().map(Box::new);
        reader.checkpoint(0, Some(completed.clone()));
        match alter {
            0 => (),
            1 => completed.connection_work = None,
            _ => completed.connection_work.as_mut().unwrap().attempts[0].reader_generation = Some("8".into()),
        }
        assert!(completed.is_valid());
        reader.checkpoint(0, Some(completed));
        reader.setup_checkpoint(0, Some(metrics(0)));
        reader.finish(SourceShutdown::Quiescent);
        let _ = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
        let observed = owner.finish(Ok(()));
        assert!(observed.acquisition().is_valid());
        if alter == 0 {
            assert_eq!(observed.acquisition().coverage, ProviderReadCoverage::Complete);
        } else {
            assert!(gaps(observed.acquisition()).contains(&ProviderReadGap::InvalidObservation));
            assert!(observed.acquisition().readers[0].work.is_none());
        }
    }
}

#[test]
fn a_foreign_or_unobserved_call_cannot_borrow_another_invocations_route_or_native_disposition() {
    for foreign in [false, true] {
        let owner = observation();
        let other =
            ProviderReadObservation::begin("1123456789abcdef0123456789abcdef".into(), ProviderReadKind::Probe).unwrap();
        let acquisition = if foreign {
            other.operation().unwrap().source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()))
        } else {
            None
        };
        let call = ProviderCall::completed(Ok("original successful provider value"), Ok(()), acquisition);
        let attempt = owner.finish_call(Ok(call));
        assert_eq!(
            attempt.original().expect("materialized original result").as_ref().unwrap(),
            &"original successful provider value"
        );
        assert!(attempt.finalization().is_err());
        assert_eq!(attempt.acquisition().observation_id, INVOCATION);
        assert_eq!(
            attempt.acquisition().coverage,
            ProviderReadCoverage::Unavailable { reason: ProviderReadGap::InvalidObservation }
        );
        assert!(attempt.acquisition().readers.is_empty());
        assert!(attempt.into_result().is_err());
    }
}
#[test]
fn setup_included_in_the_original_reader_count_is_not_duplicated_as_a_second_namespace() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    context.complete_reader_coverage();
    let reader = context.reader(Engine::Postgres);
    reader.setup_included();
    reader.checkpoint(3, Some(metrics(3)));
    reader.finish(SourceShutdown::Quiescent);
    let acquisition = operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Ok(()));
    let attempt = owner.finish_call(Ok(ProviderCall::completed(Ok(()), Ok(()), acquisition)));
    let summary = attempt.acquisition();
    assert!(summary.is_valid());
    assert_eq!(summary.coverage, ProviderReadCoverage::Complete);
    assert_eq!(summary.readers[0].queries, Some(3));
    assert_eq!(summary.readers[0].setup_queries, None);
    assert_eq!(summary.readers[0].setup_accounting, ProviderSetupAccounting::IncludedInReaderWork);
}

#[test]
fn changed_client_clock_order_cannot_remain_complete_but_keeps_explicit_unknown_clock_coverage() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    operation.context().complete_reader_coverage();
    let mut summary =
        operation.source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::NotStarted, Ok(())).unwrap();
    summary.client_started_at = "2026-11-21T00:00:01Z".into();
    summary.client_finished_at = "2026-11-21T00:00:00Z".into();
    assert!(!summary.is_valid());
    summary.coverage = ProviderReadCoverage::Partial { gaps: vec![ProviderReadGap::ClientClockChanged] };
    assert!(summary.is_valid());
    assert!(!summary.has_ordered_client_clocks());
    summary.coverage = ProviderReadCoverage::Partial { gaps: vec![ProviderReadGap::PartialReaderCoverage] };
    assert!(!summary.is_valid());
}

#[test]
fn successful_acquisition_withheld_after_failed_finalization_never_fabricates_an_original_error_or_maps_output() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    operation.admitted();
    operation.native_started();
    let failure = CollectionStop::StorageFailed.error();
    let summary = operation
        .source_finished(&Ok::<(), Error>(()), ProviderReadDisposition::Quiescent, Err(failure.clone()))
        .unwrap();
    let mapped = std::cell::Cell::new(false);
    let call = ProviderCall::<u32>::unmaterialized_after_finalization(failure, Some(summary)).map(|value| {
        mapped.set(true);
        value.to_string()
    });
    assert!(!mapped.get());
    let attempt = owner.finish_call(Ok(call));
    assert!(attempt.original().is_none());
    assert!(attempt.original_failure().is_none());
    assert_eq!(attempt.acquisition().original_failure, None);
    assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Quiescent);
    assert_eq!(attempt.acquisition().finalization_failure.as_ref().unwrap().stop, Some(CollectionStop::StorageFailed));
    assert_eq!(CollectionStop::from_error(&attempt.into_result().unwrap_err()), CollectionStop::StorageFailed);
}
#[test]
fn genuine_identical_original_and_finalization_errors_remain_two_real_errors() {
    let owner = observation();
    let operation = owner.operation().unwrap();
    let original = CollectionStop::StorageFailed.error();
    let finalization = original.clone();
    let summary = operation
        .source_finished(&Err::<(), _>(original.clone()), ProviderReadDisposition::Quiescent, Err(finalization.clone()))
        .unwrap();
    let call = ProviderCall::<()>::completed(Err(original.clone()), Err(finalization), Some(summary));
    let attempt = owner.finish_call(Ok(call));
    assert_eq!(
        serde_json::to_value(attempt.original_failure().unwrap()).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert!(attempt.acquisition().original_failure.is_some());
    assert!(attempt.acquisition().finalization_failure.is_some());
    assert!(attempt.into_result().is_err());
}
#[test]
fn unmaterialized_output_requires_successful_acquisition_and_real_failed_finalization() {
    for original_failed in [false, true] {
        let owner = observation();
        let operation = owner.operation().unwrap();
        let failure = CollectionStop::StorageFailed.error();
        let original = if original_failed { Err(CollectionStop::ByteLimit.error()) } else { Ok(()) };
        let finalization = if original_failed { Err(failure.clone()) } else { Ok(()) };
        let summary = operation.source_finished(&original, ProviderReadDisposition::Quiescent, finalization).unwrap();
        let attempt =
            owner.finish_call(Ok(ProviderCall::<()>::unmaterialized_after_finalization(failure, Some(summary))));
        assert_eq!(
            attempt.acquisition().coverage,
            ProviderReadCoverage::Unavailable { reason: ProviderReadGap::InvalidObservation }
        );
        assert_eq!(attempt.acquisition().native_disposition, ProviderReadDisposition::Unconfirmed);
        assert!(attempt.into_result().is_err());
    }
}
