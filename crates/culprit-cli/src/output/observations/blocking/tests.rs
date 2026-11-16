use super::*;
use culprit_collector::analytical::test_support::{BlockingCase, blocking_evidence};
use culprit_core::{SourceState, telemetry::*};

fn report(case: BlockingCase) -> BlockingReport {
    let (source, rows) = blocking_evidence(case);
    let rows = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Activity(observation) = row else { panic!("canonical native Activity corpus") };
            ActivityFrameRow { observation, reference: None }
        })
        .collect::<Vec<_>>();
    culprit_engine::explain_blocking(&source, &rows, true).unwrap()
}

#[test]
fn prepared_zero_is_transaction_ownership_not_a_session_and_request_generation_is_never_discarded() {
    let report = report(BlockingCase::PreparedOwner);
    let text = blocking(&report);
    assert!(text.contains(&t!("evidence.cli.blocking_owner.prepared", id = "\"0\"")));
    assert!(!text.contains(&session("0", None)));
    assert!(text.contains(&report.original.record_id));
    let request = report.relationships[0].waiter.request.as_ref().unwrap();
    assert!(text.contains(&format!("{:?}", request.generation)));
    assert!(text.contains(&format!("{:?}", report.relationships[0].waiter.session.generation)));
    assert!(text.contains(&t!("view.blocking.limit.prepared")));
    assert!(text.contains(&report.investigation) && text.contains(&report.verify));
}

#[test]
fn same_native_pid_with_known_and_unknown_generation_keeps_distinct_typed_owner_context() {
    let known =
        owner(&ActivityWaitOwner::Session { id: "43".into(), generation: Some("native-backend-generation".into()) });
    let unknown = owner(&ActivityWaitOwner::Session { id: "43".into(), generation: None });
    assert_ne!(known, unknown);
    assert!(known.contains("native-backend-generation"));
    assert_eq!(unknown, t!("evidence.cli.blocking_session_unknown", id = "\"43\""));
    let text = blocking(&report(BlockingCase::HiddenOwner));
    assert!(text.contains(&t!("evidence.cli.blocking_context_unavailable")));
    assert!(!text.contains("VACUUM orders"));
}

#[test]
fn a_same_session_native_relationship_is_visible_without_a_fabricated_graph_or_healthy_summary() {
    let mut report = report(BlockingCase::Matched);
    let waiter = report.relationships[0].waiter.session.clone();
    report.relationships[0].owner = ActivityWaitOwner::Session { id: waiter.id, generation: Some(waiter.generation) };
    report.blocking = None;
    report.limitations.push(t!("view.blocking.limit.self_session"));
    let text = blocking(&report);
    assert!(text.contains(&request(&report.relationships[0].waiter)));
    assert!(text.contains(&t!("view.blocking.limit.self_session")));
    assert!(text.contains(&report.summary));
    assert!(text.contains(&t!("evidence.cli.source_partial")));
    assert_eq!(text.matches('•').count(), 1);
}

#[test]
fn text_bounds_rendering_without_changing_native_counts_or_losing_read_and_original_scope_limitations() {
    let mut report = report(BlockingCase::UnverifiedOriginal);
    // Presentation-only expansion: all native acquisition and inference remain in the shared corpus/view.
    let edge = report.relationships[0].clone();
    report.relationships = (0..8)
        .map(|ordinal| {
            let mut copy = edge.clone();
            copy.waiter.request.as_mut().unwrap().id = ordinal.to_string();
            copy
        })
        .collect();
    let graph = report.blocking.as_mut().unwrap();
    let chain = graph.chains[0].clone();
    graph.chains = (0..8)
        .map(|ordinal| {
            let mut copy = chain.clone();
            copy.tree.key = format!("controlled-graph-key-{ordinal}");
            copy.headline = format!("controlled-chain-{ordinal}");
            copy.advice = "é".repeat(4096);
            copy
        })
        .collect();
    report.limitations.push(t!("view.blocking.limit.read"));
    let text = blocking(&report);
    assert_eq!(text.matches('•').count(), 2 * super::super::super::BRIEF_FINDING_LIMIT);
    assert!(text.contains(&t!(
        "evidence.cli.blocking_counts",
        native = "2",
        observed = "2",
        retained = "8",
        shown = "3"
    )));
    assert!(text.contains(&t!("evidence.cli.blocking_chains", retained = "8", shown = "3")));
    assert!(!text.contains("controlled-chain-3"));
    assert!(text.contains(&t!("view.blocking.limit.original_scope")));
    assert!(text.contains(&t!("view.blocking.limit.read")));
    assert!(text.len() < 32 * 1024);
}

#[test]
fn actual_archive_row_loss_and_native_unavailability_remain_unknown_with_independent_coverage() {
    let (source, _) = blocking_evidence(BlockingCase::Matched);
    let missing = culprit_engine::explain_blocking(&source, &[], false).unwrap();
    let text = blocking(&missing);
    assert!(!missing.read_complete);
    assert_eq!(missing.source_state, SourceState::Partial);
    assert!(text.contains(&t!("view.blocking.summary.read_gap")));
    assert!(text.contains(&t!(
        "evidence.cli.blocking_counts",
        native = "2",
        observed = "0",
        retained = "0",
        shown = "0"
    )));
    let unavailable = blocking(&report(BlockingCase::Unavailable));
    assert!(unavailable.contains(&t!("evidence.cli.source_unavailable")));
    assert!(unavailable.contains(&t!("view.blocking.summary.unavailable")));
    let ended = report(BlockingCase::RequestMissing);
    assert!(blocking(&ended).contains(&t!("view.blocking.limit.request_end")));
}
