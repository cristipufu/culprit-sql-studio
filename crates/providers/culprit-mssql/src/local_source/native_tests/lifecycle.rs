//! No waits or extra operators are appended to the source template to manufacture cancellation coverage.
use super::*;
use crate::read::measurement::{assert_no_followup, shutdown_state};
use std::path::Path;

pub(super) async fn run(fixture: &OwnedSourceFixture, directory: &Path) -> Result<()> {
    expired_before_admission(fixture, directory).await?;
    deadline(fixture, directory).await?;
    if fixture.pool.source_shutdown() == SourceShutdown::Unconfirmed {
        return save_cost_receipt(
            directory,
            "context-cancellation-not-run",
            &json!({
                "state":"not-run","reason":"source pool remains fenced after deadline settlement; no replacement connection or source work",
                "production_admission":"unqualified",
            }),
        );
    }
    caller_cancellation(fixture, directory).await
}

async fn expired_before_admission(fixture: &OwnedSourceFixture, directory: &Path) -> Result<()> {
    let budget = CollectionBudget { statement_timeout_ms: 1, total_timeout_ms: 1, ..source_budget() };
    let mut reader = measured(fixture, budget).await?;
    tokio::time::sleep(Duration::from_millis(3)).await;
    let result = Context::read(&mut reader, "127.0.0.1", fixture.port).await;
    let shutdown = reader.close().await;
    let quiet = reader.is_quiet().await;
    save_cost_receipt(
        directory,
        "context-deadline-before-admission",
        &json!({
            "case":"expired-before-native-admission","error":result.as_ref().err().map(|error|error.code),
            "source_queries":reader.queries(),"shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
            "exact_projection_cancellation_exercised":false,"accounting":reader.report(),"production_admission":"unqualified",
        }),
    )?;
    assert!(matches!(result,Err(ref error) if CollectionStop::from_error(error)==CollectionStop::TimedOut));
    assert_eq!(reader.queries(), 0, "an expired deadline cannot admit platform, epoch or projection SQL");
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(quiet?);
    assert_no_followup(&mut reader).await;
    Ok(())
}

async fn deadline(fixture: &OwnedSourceFixture, directory: &Path) -> Result<()> {
    let budget = CollectionBudget { statement_timeout_ms: 1, total_timeout_ms: 1000, ..source_budget() };
    let mut reader = measured(fixture, budget).await?;
    let projection = digest(query(IdentityDetail::Endpoint, Detail::Authority).as_bytes());
    let observation = reader.watch_template(&projection)?;
    let result = Context::read(&mut reader, "127.0.0.1", fixture.port).await;
    let shutdown = reader.close().await;
    let quiet = reader.is_quiet().await;
    let metrics = reader.read_metrics().expect("deadline accounting");
    let admitted = metrics.templates.iter().any(|metric| metric.template_id == projection);
    let timed_out = result.as_ref().err().is_some_and(|error| {
        matches!(CollectionStop::from_error(error), CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed)
    });
    save_cost_receipt(
        directory,
        "context-native-deadline",
        &json!({
            "case":"unchanged-context-statement-deadline","projection_template_id":projection,
            "projection_admitted":admitted,"exact_projection_active_observed":observation.was_active(),
            "exact_projection_timeout_exercised":timed_out && observation.was_active(),
            "error":result.as_ref().err().map(|error|error.code),"completed_before_deadline":result.is_ok(),
            "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
            "accounting":reader.report(),"production_admission":"unqualified",
            "limitation":"a pre-admission/other-phase timeout or a fast completed projection does not qualify active cancellation of this template",
        }),
    )?;
    if let Err(error) = &result {
        assert!(
            matches!(
                CollectionStop::from_error(error),
                CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed
            ),
            "only the deliberate deadline may stop this owned read"
        );
    }
    assert!(quiet?);
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader).await;
    }
    Ok(())
}

enum CancellationAttempt {
    Completed(Result<()>),
    DroppedAfterExactTemplateObservation,
    ObservationDeadline,
}

async fn caller_cancellation(fixture: &OwnedSourceFixture, directory: &Path) -> Result<()> {
    let mut reader = measured(fixture, source_budget()).await?;
    let projection = digest(query(IdentityDetail::Endpoint, Detail::Authority).as_bytes());
    let observation = reader.watch_template(&projection)?;
    let attempt = {
        let work = Context::read(&mut reader, "127.0.0.1", fixture.port);
        tokio::pin!(work);
        let observation_deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(observation_deadline);
        loop {
            tokio::select! {
                result=&mut work=>break CancellationAttempt::Completed(result.map(|_| ())),
                _=&mut observation_deadline=>break CancellationAttempt::ObservationDeadline,
                _=tokio::time::sleep(Duration::from_millis(1))=>{
                    if observation.was_active() {break CancellationAttempt::DroppedAfterExactTemplateObservation;}
                }
            }
        }
    };
    // The borrowed request future is gone before close assumes sole ownership of its native task.
    let shutdown = reader.close().await;
    let quiet = reader.is_quiet().await;
    let exercised = matches!(attempt, CancellationAttempt::DroppedAfterExactTemplateObservation);
    let state = match &attempt {
        CancellationAttempt::Completed(Ok(_)) => "completed-too-fast-to-cancel",
        CancellationAttempt::Completed(Err(_)) => "failed-before-caller-cancellation",
        CancellationAttempt::DroppedAfterExactTemplateObservation => "caller-dropped-after-exact-template-observation",
        CancellationAttempt::ObservationDeadline => "active-template-not-established-before-observation-deadline",
    };
    save_cost_receipt(
        directory,
        "context-caller-cancellation",
        &json!({
            "case":state,"projection_template_id":projection,"exact_projection_active_observed":observation.was_active(),
            "exact_projection_caller_cancellation_exercised":exercised,
            "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
            "accounting":reader.report(),"production_admission":"unqualified",
            "limitation":"no causal cancellation effect or template cancellation guarantee is inferred when the native request may have completed before close",
        }),
    )?;
    if let CancellationAttempt::Completed(result) = attempt {
        result?;
    }
    assert!(!exercised || observation.was_active());
    assert!(quiet?);
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader).await;
    }
    Ok(())
}
