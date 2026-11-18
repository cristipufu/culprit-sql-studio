//! Every case uses the exact production template. Fast completion never counts as active cancellation proof.
use super::*;

#[derive(Clone, Copy)]
enum Case {
    Expired,
    QueryCeiling,
    ByteCeiling,
    Deadline,
    CallerCancellation,
    RowCeiling,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Expired => "expired-before-admission",
            Self::QueryCeiling => "query-ceiling",
            Self::ByteCeiling => "byte-ceiling",
            Self::Deadline => "native-deadline",
            Self::CallerCancellation => "caller-cancellation",
            Self::RowCeiling => "row-ceiling",
        }
    }
    fn budget(self) -> CollectionBudget {
        match self {
            Self::Expired => CollectionBudget { statement_timeout_ms: 1, total_timeout_ms: 1, ..source_budget() },
            Self::ByteCeiling => CollectionBudget { page_bytes: 1024, ..source_budget() },
            Self::Deadline => CollectionBudget { statement_timeout_ms: 1, total_timeout_ms: 1000, ..source_budget() },
            _ => source_budget(),
        }
    }
}
const CASES: [Case; 6] =
    [Case::Expired, Case::QueryCeiling, Case::ByteCeiling, Case::Deadline, Case::CallerCancellation, Case::RowCeiling];

pub(super) async fn run(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    for case in CASES {
        if fixture.pool.source_shutdown() == SourceShutdown::Unconfirmed {
            save_cost_receipt(
                directory,
                "performance-limits-remaining-unqualified",
                &json!({
                    "next_case":case.name(), "state":"not-run",
                    "reason":"the original source pool remains fenced; no replacement reader or source statement is admitted",
                    "production_admission":"unqualified", "native_cpu_io_cost_qualified":false,
                }),
            )?;
            break;
        }
        if matches!(case, Case::CallerCancellation) {
            caller_cancellation(fixture, directory, cancellation).await?;
        } else {
            bounded(fixture, directory, cancellation, case).await?;
        }
    }
    Ok(())
}

async fn bounded(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
    case: Case,
) -> Result<()> {
    let mut reader = measured(fixture, case.budget(), cancellation).await?;
    let watch = reader.watch_template(&digest(sql().as_bytes()))?;
    if matches!(case, Case::Expired) {
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    let limit = if matches!(case, Case::RowCeiling) { 1 } else { MAX_COUNTERS };
    let result = until_cancelled(cancellation, read(&mut reader, limit)).await;
    let metrics_before_refusal = reader.read_metrics();
    let repeat = if matches!(case, Case::QueryCeiling) && result.is_ok() {
        Some(until_cancelled(cancellation, read(&mut reader, MAX_COUNTERS)).await)
    } else {
        None
    };
    let settlement = settle(&mut reader).await;
    let stop = result.as_ref().err().map(CollectionStop::from_error);
    let mut receipt = settlement.receipt();
    receipt["case"] = json!(case.name());
    receipt["template_id"] = json!(digest(sql().as_bytes()));
    receipt["source_template"] = json!(true);
    receipt["tested_budget"] = json!(case.budget());
    receipt["requested_counter_limit"] = json!(limit);
    receipt["native_row_lookahead_ceiling"] = json!(limit + 1);
    receipt["source_result"] = error_receipt(&result);
    receipt["second_source_result"] = repeat.as_ref().map(error_receipt).unwrap_or(Value::Null);
    receipt["actual_template_active_observed"] = json!(watch.was_active());
    receipt["active_template_deadline_exercised"] = json!(
        matches!(case, Case::Deadline)
            && watch.was_active()
            && settlement
                .metrics
                .as_ref()
                .is_some_and(|metrics| metrics.templates.iter().any(|metric| metric.timeouts > 0))
    );
    receipt["byte_limit_exercised"] =
        json!(matches!(case, Case::ByteCeiling) && stop == Some(CollectionStop::ByteLimit));
    receipt["row_limit_exercised"] = json!(matches!(case, Case::RowCeiling) && stop == Some(CollectionStop::RowLimit));
    receipt["limits"] = json!(
        "row/byte ceilings bound retained response; a charged over-limit row may be recorded before rejection; no returned prefix is a successful observation"
    );
    if let Ok(read) = &result {
        receipt["native_input_rows"] = json!(read.native_rows);
        receipt["aggregate_observations"] = json!(observations(read.clone())?.len());
    }
    save_cost_receipt(directory, &format!("performance-{}", case.name()), &receipt)?;
    settlement.confirm_quiet()?;
    assert!(settlement.queries <= 1, "no retries or replacement source SQL");
    if matches!(case, Case::Expired) {
        assert_eq!(stop, Some(CollectionStop::TimedOut));
        assert_eq!(settlement.queries, 0);
        assert!(!watch.was_active());
        assert_eq!(settlement.shutdown, SourceShutdown::Quiescent);
    } else if matches!(case, Case::QueryCeiling) {
        result?;
        let repeated = repeat.expect("the genuine first read must precede query-budget refusal");
        assert_eq!(CollectionStop::from_error(&repeated.expect_err("query limit refusal")), CollectionStop::QueryLimit);
        assert_eq!(metrics_before_refusal, settlement.metrics, "refused admission does not change native metrics");
        assert_eq!(settlement.queries, 1);
    } else if matches!(case, Case::RowCeiling) {
        assert!(matches!(stop, Some(CollectionStop::RowLimit | CollectionStop::CancellationUnconfirmed)));
        assert!(result.is_err(), "the native six-counter response cannot fit a one-counter cap");
    } else if let Err(error) = result {
        let permitted = match case {
            Case::ByteCeiling => matches!(
                CollectionStop::from_error(&error),
                CollectionStop::ByteLimit | CollectionStop::CancellationUnconfirmed
            ),
            Case::Deadline => matches!(
                CollectionStop::from_error(&error),
                CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed
            ),
            _ => false,
        };
        if !permitted {
            return Err(error);
        }
    } else {
        let metrics = settlement.metrics.as_ref().expect("settled source metrics");
        let bytes =
            metrics.templates.iter().map(|metric| metric.charged_value_bytes.parse::<u64>().unwrap()).sum::<u64>();
        assert!(bytes <= u64::from(case.budget().page_bytes));
        assert_eq!(settlement.shutdown, SourceShutdown::Quiescent);
    }
    Ok(())
}

enum CancellationAttempt {
    Completed(Result<PerformanceRead>),
    ExactTemplateObserved,
    ObservationDeadline,
    CaseCancelled,
}

async fn caller_cancellation(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let mut reader = measured(fixture, source_budget(), cancellation).await?;
    let watch = reader.watch_template(&digest(sql().as_bytes()))?;
    let attempt = {
        let work = read(&mut reader, MAX_COUNTERS);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result = &mut work => break CancellationAttempt::Completed(result),
                _ = cancellation.cancelled() => break CancellationAttempt::CaseCancelled,
                _ = &mut deadline => break CancellationAttempt::ObservationDeadline,
                _ = tokio::time::sleep(Duration::from_millis(1)) => {
                    if watch.was_active() { break CancellationAttempt::ExactTemplateObserved; }
                }
            }
        }
    };
    // Only the borrowed query future ends here. The same MeasuredRead retains and settles the native owner.
    let settlement = settle(&mut reader).await;
    let exercised = matches!(attempt, CancellationAttempt::ExactTemplateObserved);
    let state = match &attempt {
        CancellationAttempt::Completed(Ok(_)) => "completed-too-fast-to-cancel",
        CancellationAttempt::Completed(Err(_)) => "failed-before-caller-cancellation",
        CancellationAttempt::ExactTemplateObserved => "caller-dropped-after-exact-template-observation",
        CancellationAttempt::ObservationDeadline => "active-template-not-established-before-observation-deadline",
        CancellationAttempt::CaseCancelled => "case-deadline-cancellation",
    };
    let mut receipt = settlement.receipt();
    receipt["case"] = json!(state);
    receipt["template_id"] = json!(digest(sql().as_bytes()));
    receipt["source_template"] = json!(true);
    receipt["actual_template_active_observed"] = json!(watch.was_active());
    receipt["caller_dropped_after_exact_active_observation"] = json!(exercised);
    receipt["active_template_caller_cancellation_exercised"] = json!(
        exercised
            && settlement
                .metrics
                .as_ref()
                .is_some_and(|metrics| metrics.templates.iter().any(|metric| metric.cancellations > 0))
    );
    receipt["limitation"] = json!(
        "a completed fast query or observer timing cannot establish successful active cancellation, native cost or a server-work bound"
    );
    if let CancellationAttempt::Completed(result) = &attempt {
        receipt["source_result"] = error_receipt(result);
    }
    save_cost_receipt(directory, "performance-caller-cancellation", &receipt)?;
    settlement.confirm_quiet()?;
    assert!(settlement.queries <= 1);
    assert!(!exercised || watch.was_active());
    match attempt {
        CancellationAttempt::Completed(result) => {
            result?;
        }
        CancellationAttempt::CaseCancelled => return Err(CollectionStop::Cancelled.error()),
        _ => (),
    }
    Ok(())
}
