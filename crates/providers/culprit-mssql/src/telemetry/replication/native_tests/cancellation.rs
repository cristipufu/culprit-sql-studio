use super::*;

const OBSERVATION_POLL: Duration = Duration::from_millis(1);

pub(super) async fn observe(
    fixture: &OwnedSourceFixture,
    template: Option<&str>,
    budget: &CollectionBudget,
) -> Result<Value> {
    let Some(template) = template else {
        return Ok(json!({
            "case":"actual-expanded-ag-cancellation","state":"unverified",
            "reason":"expanded AG SELECT was not exercised; no substitute statement was run",
            "active_template_observed":false,"native_cpu_io_cost_qualified":false,
            "production_admission":"unqualified",
        }));
    };
    let profile_started = Instant::now();
    let mut reader = capture::open(fixture, budget.clone()).await?;
    let scope = reader.io.scope().clone();
    let observed = match reader.io.watch_template(template) {
        Ok(observed) => observed,
        Err(error) => {
            let _ = reader.close().await;
            return Err(error);
        }
    };
    let mut completed = None;
    let mut requested = false;
    let mut wall_deadline = false;
    let request = request(budget.clone());
    let source_started = Instant::now();
    {
        let work = reader.observe(&request);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(Duration::from_millis(u64::from(budget.total_timeout_ms)));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result = &mut work => { completed = Some(result); break; }
                _ = tokio::time::sleep(OBSERVATION_POLL) => {
                    if observed.was_active() { requested = true; break; }
                }
                _ = &mut deadline => { wall_deadline = true; break; }
            }
        }
        // Dropping the actual source future requests cancellation through its original ReadSession.
    }
    let close_started = Instant::now();
    let shutdown = reader.close().await;
    let close_elapsed_ms = close_started.elapsed().as_secs_f64() * 1000.0;
    let source_elapsed_ms = source_started.elapsed().as_secs_f64() * 1000.0;
    let quiet = reader.io.is_quiet().await?;
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader.io).await;
    }
    let metrics = reader.read_metrics();
    let measured =
        metrics.as_ref().and_then(|metrics| metrics.templates.iter().find(|phase| phase.template_id == template));
    let cancellations = measured.map_or(0, |phase| phase.cancellations);
    let unconfirmed = measured.map_or(0, |phase| phase.unconfirmed_cancellations);
    Ok(json!({
        "case":"actual-expanded-ag-cancellation",
        "state":classification(observed.was_active(), requested, cancellations, unconfirmed, shutdown, quiet),
        "scope":scope,"tested_budget":budget,"template_id":template,
        "profile_including_preparation_client_elapsed_ms":profile_started.elapsed().as_secs_f64()*1000.0,
        "source_with_observer_and_close_client_elapsed_ms":source_elapsed_ms,
        "close_client_elapsed_ms":close_elapsed_ms,
        "active_template_observed":observed.was_active(),"cancellation_requested":requested,
        "client_wall_deadline_reached":wall_deadline,
        "matching_template_cancellations":cancellations,"matching_template_unconfirmed_cancellations":unconfirmed,
        "source_returned_before_cancellation":completed.is_some(),
        "source_error":completed.as_ref().and_then(|result|result.as_ref().err()).map(|error|error.code),
        "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet,
        "published":false,"accounting":reader.io.report(),
        "native_cpu_io_cost_qualified":false,"production_admission":"unqualified",
    }))
}

pub(super) fn classification(
    observed: bool,
    requested: bool,
    cancellations: u32,
    unconfirmed: u32,
    shutdown: SourceShutdown,
    quiet: bool,
) -> &'static str {
    if !observed || !requested {
        "unverified"
    } else if shutdown == SourceShutdown::Unconfirmed || unconfirmed > 0 {
        "unconfirmed"
    } else if cancellations > 0 && quiet {
        "acknowledged"
    } else {
        // An observed phase may finish between the sample and the cancellation request.
        "unverified"
    }
}
