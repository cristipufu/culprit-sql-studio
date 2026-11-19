use super::*;
use culprit_collector::analytical::test_support::{PageWriterCase, page_writer_evidence};
use culprit_core::{PageWaitRecordedIncident, SourceState, telemetry::*};

fn report(sql: DefinitionPayload) -> PageWaitReport {
    let (source, _) = page_writer_evidence(PageWriterCase::TargetWriter);
    let SourceObservation::PageWaitWriters { focus, .. } = source.observation else { panic!() };
    let original = focus.waiter.participant();
    PageWaitReport {
        source_state: Some(SourceState::Empty),
        source_reason: None,
        title: "Page waits".into(),
        summary: "No current target rows were returned.".into(),
        recorded_incident: Some(PageWaitRecordedIncident {
            original: original.parent.clone(),
            observed_at: original.original_observed_at.clone(),
            request: original.original.key.clone(),
            resource: focus.waiter.resource.clone(),
            sql,
            client: Some(ActivityClient {
                application: Some("recorded-application".into()),
                user: Some("recorded-login".into()),
                host: None,
            }),
        }),
        limitations: vec!["Current ownership needs independent evidence.".into()],
        entries: vec![],
    }
}

#[test]
fn an_empty_current_capture_keeps_recorded_sql_client_and_time_without_native_proof_details() {
    let report = report(DefinitionPayload::sql(
        "SELECT * FROM dbo.recorded_waiter WHERE id = 8765",
        culprit_core::Engine::Mssql,
    ));
    let incident = report.recorded_incident.as_ref().unwrap();
    let output = page_waits(&report);
    assert!(output.contains(&t!("view.page_waits.recorded.title")));
    assert!(output.contains(&incident.observed_at));
    assert!(output.contains("dbo.recorded_waiter"));
    assert!(output.contains("recorded-application") && output.contains("recorded-login"));
    assert!(output.contains(&t!("view.page_waits.recorded.qualification")));
    assert!(!output.contains("8765"));
    assert!(!output.contains(&incident.original.manifest_sha256));
    assert!(!output.contains(&incident.original.record_id));
    assert!(report.entries.is_empty());
}

#[test]
fn every_recorded_sql_unavailability_stays_explicit_without_a_fallback_statement() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let payload = DefinitionPayload::Unavailable { reason };
        let expected = statement::sql(Some(&payload));
        let output = page_waits(&report(payload));
        assert!(output.contains(&expected));
        assert!(!output.contains("dbo.recorded_waiter"));
    }
}

#[test]
fn absent_authenticated_originals_do_not_create_recorded_context_or_sql() {
    let mut report = report(DefinitionPayload::sql("SELECT * FROM dbo.recorded_waiter", culprit_core::Engine::Mssql));
    report.recorded_incident = None;
    let output = page_waits(&report);
    assert!(!output.contains(&t!("view.page_waits.recorded.title")));
    assert!(!output.contains("dbo.recorded_waiter") && !output.contains("recorded-application"));
    assert!(output.contains(&report.summary));
}

#[test]
fn recorded_excerpt_and_client_values_are_bounded_and_terminal_control_text_is_escaped() {
    let mut report = report(DefinitionPayload::sql(
        &format!("SELECT {} FROM dbo.hidden_tail", "列".repeat(1000)),
        culprit_core::Engine::Mssql,
    ));
    report.recorded_incident.as_mut().unwrap().client.as_mut().unwrap().application =
        Some(format!("label\n\x1b[31m{}", "x".repeat(500)));
    let output = page_waits(&report);
    assert!(!output.contains("dbo.hidden_tail"));
    assert!(output.contains("label\\n\\u{1b}[31m"));
    assert!(!output.contains('\x1b'));
    assert!(!output.contains(&"x".repeat(101)));
    assert!(output.contains('…'));
}
