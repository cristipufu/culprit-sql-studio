use super::*;
fn focus() -> ActivityWaitFocus {
    crate::activity::page_targets::focus::tests::focus(&crate::activity::page_targets::tests::page_wait(
        42,
        "PAGELATCH_EX",
        "7:1:4144",
    ))
}
fn result(focus: &ActivityWaitFocus) -> ActivityWaitFocusResult {
    ActivityWaitFocusResult {
        focus: focus.clone(),
        observed_from: focus.participant.original_observed_at.clone(),
        observed_until: focus.participant.original_observed_at.clone(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    }
}
#[test]
fn ended_control_requires_the_same_original_and_known_source_without_current_rows_or_page_permission() {
    let focus = focus();
    for request in [PageWaitRequestState::Missing, PageWaitRequestState::Changed] {
        let mut result = result(&focus);
        result.request = request;
        require_ended_focus(&focus, &result, 0, 0).unwrap();
    }
    for mutation in ["source", "request", "wait", "receipt", "native-rows", "body"] {
        let mut result = result(&focus);
        let mut native_rows = 0;
        let mut rows = 0;
        match mutation {
            "source" => result.source = FocusSourceState::Unverified,
            "request" => result.request = PageWaitRequestState::SameStatement,
            "wait" => result.wait = PageWaitRelation::OriginalObserved,
            "receipt" => result.focus.participant.parent.manifest_sha256 = digest(b"foreign original"),
            "native-rows" => native_rows = 1,
            _ => rows = 1,
        }
        assert!(require_ended_focus(&focus, &result, native_rows, rows).is_err(), "{mutation}");
    }
}
#[test]
fn retained_original_expectation_redacts_only_original_sql_and_refuses_any_later_context() {
    let focus = focus();
    let expected = match &focus.participant.original.statement {
        DefinitionPayload::Available { text, .. } => DefinitionPayload::sql(text, focus.participant.target.engine()),
        unavailable @ DefinitionPayload::Unavailable { .. } => unavailable.clone(),
    };
    let mut report = culprit_checks::page_waits::explain(&[], None, None).unwrap();
    report.recorded_incident = Some(culprit_core::page_wait_report::PageWaitRecordedIncident {
        original: focus.participant.parent.clone(),
        observed_at: focus.participant.original_observed_at.clone(),
        request: focus.participant.original.key.clone(),
        resource: focus.resource.clone(),
        sql: expected,
        client: None,
    });
    require_original_incident(&focus, &report).unwrap();
    report.recorded_incident.as_mut().unwrap().original.manifest_sha256 = digest(b"later context");
    assert!(require_original_incident(&focus, &report).is_err());
}

#[test]
fn unavailable_template_previews_never_become_a_zero_enrichment_or_native_cost_claim() {
    let recorder = culprit_core::telemetry::ReadMetricsRecorder::new(culprit_core::Engine::Mssql);
    let id = recorder.begin("SELECT 1");
    recorder.finish(id.as_deref(), Duration::ZERO, &Default::default(), &Default::default());
    let mut metrics = recorder.snapshot().unwrap();
    let safe = "SELECT 1";
    metrics.templates[0].statement = DefinitionPayload::Available {
        text: safe.into(),
        sha256: digest(safe.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    assert_eq!(enrichment_observed(&metrics), Some(false));
    metrics.templates[0].statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    assert_eq!(enrichment_observed(&metrics), None);
    let forbidden = "SELECT page_id FROM sys.dm_db_page_info(7,1,42,'LIMITED')";
    metrics.templates[0].statement = DefinitionPayload::Available {
        text: forbidden.into(),
        sha256: digest(forbidden.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    assert_eq!(enrichment_observed(&metrics), Some(true));
}
