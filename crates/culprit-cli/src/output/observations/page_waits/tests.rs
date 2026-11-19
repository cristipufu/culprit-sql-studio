use super::*;
use culprit_collector::analytical::test_support::{PageWriterCase, page_writer_evidence};
use culprit_core::{
    PageWaitExplanation, PageWaitRecordedIncident, PageWaitRecordedRequest, PageWaitTaskReference, SourceState,
    telemetry::*,
};

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

fn task_report(sql: DefinitionPayload) -> PageWaitReport {
    let mut report = report(sql);
    let original = report.recorded_incident.take().unwrap();
    report.source_state = Some(SourceState::Collected);
    report.summary = "One retained task wait; earlier statement context is separate.".into();
    report.entries.push(PageWaitExplanation {
        observation_key: "private-later-task-reference".into(),
        request_key: original.request.clone(),
        task: Some(PageWaitTaskReference { task_address: "0000000000000001".into(), execution_context_id: 1 }),
        resource: culprit_core::ObservedWaitResource {
            kind: culprit_core::WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("5:1:84"),
        },
        recorded_request: Some(PageWaitRecordedRequest {
            original: original.original,
            observed_at: original.observed_at,
            request: original.request,
            sql: original.sql,
            client: original.client,
        }),
        blocking: culprit_core::PageWaitBlockingParticipation::unavailable(),
        title: "Observed task wait".into(),
        explanation: "The task waited on its later resource.".into(),
        investigation: "Keep earlier context distinct.".into(),
        verify: "Compare native observations.".into(),
    });
    report
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

#[test]
fn task_sql_uses_its_earlier_context_without_borrowing_a_resource_or_exposing_private_receipts() {
    let report = task_report(DefinitionPayload::sql(
        "SELECT earlier_marker FROM dbo.orders WHERE customer = 8821",
        culprit_core::Engine::Mssql,
    ));
    let context = report.entries[0].recorded_request.as_ref().unwrap();
    let output = page_waits(&report);
    assert!(output.contains(&t!("view.page_waits.recorded_request.title")));
    assert!(output.contains(&t!("view.page_waits.recorded_request.qualification")));
    assert!(output.contains(&context.observed_at));
    assert!(output.contains("earlier_marker") && output.contains("recorded-application"));
    assert!(output.contains("5:1:84"));
    assert!(!output.contains("5:1:42"));
    assert!(!output.contains(&t!("view.page_waits.recorded.title")));
    assert!(!output.contains("8821"));
    assert!(!output.contains(&context.original.manifest_sha256));
    assert!(!output.contains(&context.original.record_id));
    assert!(!output.contains("private-later-task-reference"));
}

#[test]
fn earlier_task_unavailability_is_preserved_without_a_fallback_statement() {
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
        let output = page_waits(&task_report(payload));
        assert!(output.contains(&expected));
        assert!(output.contains(&t!("view.page_waits.recorded_request.qualification")));
        assert!(!output.contains("earlier_marker"));
    }
}

#[test]
fn task_request_output_preserves_common_count_and_unicode_detail_limits() {
    let mut report = task_report(DefinitionPayload::sql(
        &format!("SELECT {} FROM hidden_sql_tail", "列".repeat(1000)),
        culprit_core::Engine::Mssql,
    ));
    let mut entry = report.entries.remove(0);
    let request = entry.recorded_request.as_mut().unwrap();
    request.client.as_mut().unwrap().application = Some(format!("label\n\x1b[31m{}", "x".repeat(1000)));
    for index in 0..8 {
        let mut row = entry.clone();
        row.title = format!("task-{index}");
        report.entries.push(row);
    }
    let output = page_waits(&report);
    assert_eq!(output.lines().filter(|line| line.starts_with("• ")).count(), super::super::super::BRIEF_FINDING_LIMIT);
    assert!(!output.contains("task-3") && !output.contains("hidden_sql_tail"));
    assert!(!output.contains('\x1b'));
    assert!(output.contains("label\\n\\u{1b}[31m"));
    assert!(!output.contains(&"x".repeat(101)));
    assert!(output.chars().count() < 4 * super::super::OBSERVATION_DETAIL_CHARS);
}
