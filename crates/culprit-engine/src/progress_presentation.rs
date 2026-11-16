//! Native progress is validated and worded after the provider's read owner finishes.
use culprit_core::{Error, QueryProgress, Result, t};

pub(super) fn present(mut progress: QueryProgress) -> Result<QueryProgress> {
    progress.current_work_report = None;
    validate_session_point(&progress)?;
    if progress.session_context.is_some() {
        progress.request_key = None;
    }
    explain_current_work(&mut progress)?;
    if progress.running != Some(true) {
        progress.request_key = None;
        progress.plan = None;
        progress.nodes.clear();
    }
    if let Some(plan) = &mut progress.plan {
        culprit_checks::present_progress_plan(plan);
    }
    Ok(progress)
}

fn validate_session_point(progress: &QueryProgress) -> Result<()> {
    let Some(point) = &progress.session_context else { return Ok(()) };
    if !point.is_valid()
        || point.context.target.engine() != progress.engine
        || point.session != progress.session
        || progress.current_work.is_some()
        || progress.plan.is_some()
        || !progress.nodes.is_empty()
        || progress.percent.is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

fn explain_current_work(progress: &mut QueryProgress) -> Result<()> {
    let Some(captured) = &progress.current_work else { return Ok(()) };
    if progress.engine != captured.context.target.engine()
        || captured
            .observations
            .iter()
            .any(|point| progress.engine.spec().live_session_id(&point.actor.session) != progress.session)
    {
        return Err(invalid());
    }
    let report = culprit_checks::current_request_work::explain(captured, true)?;
    progress.request_key = if progress.running == Some(true) {
        captured
            .observations
            .first()
            .and_then(|point| captured.context.request_key(&point.actor, &point.statement_identity))
    } else {
        None
    };
    progress.current_work_report = Some(Box::new(report));
    Ok(())
}

fn invalid() -> Error {
    Error::invalid(t!("collection.invalid_source"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{DatabaseIdentity, Engine, NodeProgress, NodeState, SessionProgressContext, telemetry::*};

    fn session_point() -> SessionProgressContext {
        SessionProgressContext {
            source: "information_schema.PROCESSLIST".into(),
            context: NativeReadContext {
                target: DatabaseIdentity::server(Engine::Mysql, "db", "owned.invalid", 3306, None).into(),
                partition: SourcePartition::InstanceLocal { member: None, epoch: None },
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                database_writable: None,
            },
            observed_at: "2020-01-01T12:00:00.5Z".into(),
            session: "42".into(),
            database: "db".into(),
        }
    }

    fn stale_node() -> NodeProgress {
        NodeProgress {
            node_id: 0,
            state: NodeState::Running,
            rows: 1.0,
            est_rows: 10.0,
            elapsed_ms: None,
            cpu_ms: None,
            logical_reads: None,
        }
    }

    #[test]
    fn unavailable_progress_remains_unknown_and_cannot_retain_a_previous_request_cache_or_nodes() {
        let mut input = QueryProgress::unobserved("42", Engine::Mysql, "2020-01-01T12:00:01Z".into());
        input.request_key = Some("previous-native-request".into());
        input.nodes.push(stale_node());
        let output = present(input).unwrap();
        assert_eq!(output.running, None);
        assert_eq!(output.request_key, None);
        assert!(output.nodes.is_empty() && output.plan.is_none());
    }

    #[test]
    fn independent_session_data_never_inherits_request_authority_or_whole_statement_progress() {
        let mut input = QueryProgress::unobserved("42", Engine::Mysql, "2020-01-01T12:00:01Z".into());
        input.running = Some(true);
        input.session_context = Some(Box::new(session_point()));
        input.request_key = Some("borrowed-request-key".into());
        assert_eq!(present(input.clone()).unwrap().request_key, None);
        input.nodes.push(stale_node());
        assert!(present(input.clone()).is_err());
        input.nodes.clear();
        input.percent = Some(0.5);
        assert!(present(input.clone()).is_err());
        input.percent = None;
        input.session_context.as_mut().unwrap().database = "another-db".into();
        assert!(present(input).is_err());
    }

    #[test]
    fn independent_native_session_cannot_be_presented_as_a_different_session() {
        let mut input = QueryProgress::unobserved("43", Engine::Mysql, "2020-01-01T12:00:01Z".into());
        input.running = Some(true);
        input.session_context = Some(Box::new(session_point()));
        assert!(present(input).is_err());
    }

    #[test]
    fn native_current_work_cannot_supply_another_displayed_sessions_report_or_cache() {
        let (source, records) =
            culprit_collector::analytical::test_support::current_request_work_evidence_cases().remove(0);
        let SourceObservation::CurrentRequestWork { snapshot, result } = &source.observation else { panic!() };
        let [
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::CurrentRequestWork(point),
                ..
            }),
        ] = records.as_slice()
        else {
            panic!()
        };
        let session = Engine::Mysql.spec().live_session_id(&point.actor.session);
        let mut input = QueryProgress::unobserved(&session, Engine::Mysql, snapshot.observed_until.clone());
        input.running = Some(true);
        input.current_work = Some(Box::new(CurrentRequestWorkSnapshot {
            source: source.source.clone(),
            context: result.read_context(&source, snapshot),
            coverage: snapshot.sources[0].clone(),
            result: result.as_ref().clone(),
            observations: vec![point.as_ref().clone()],
        }));
        let output = present(input.clone()).unwrap();
        assert!(output.current_work_report.is_some() && output.request_key.is_some());
        input.session = format!("{session}0");
        assert!(present(input).is_err());
    }
}
