use culprit_core::{
    Engine, PageWaitExplanation, PageWaitRecordedIncident, PageWaitRecordedRequest, PageWaitReport,
    PageWaitTaskReference, Report, SourceState, telemetry::*,
};
use serde_json::{Value, json};

fn incident(sql: DefinitionPayload) -> PageWaitRecordedIncident {
    let request = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "original-login-generation".into() },
        request: Some(NativeActivityIdentity { id: "0".into(), generation: "original-request-generation".into() }),
        transaction: None,
    };
    PageWaitRecordedIncident {
        original: DiagnosticEvidenceRef {
            capture: CaptureIdentity { observation_id: "private-original-capture-canary".into(), revision: None },
            manifest_sha256: digest(b"private manifest"),
            source_scope_id: digest(b"private scope"),
            stream: TelemetryStream::ActivityObservations,
            native_key: NativeRecordIdentity::Activity(request.clone()),
            batch_id: digest(b"private batch"),
            record_id: digest(b"private semantic record"),
            payload_sha256: digest(b"private original payload"),
            representation_version: REPRESENTATION_VERSION,
            row_ordinal: 17,
        },
        observed_at: "2026-11-13T10:00:00.025Z".into(),
        request,
        resource: culprit_core::ObservedWaitResource {
            kind: culprit_core::WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("5:1:42"),
        },
        sql,
        client: Some(ActivityClient {
            application: Some("observed-app".into()),
            user: Some("observed-user".into()),
            host: Some("observed-host".into()),
        }),
    }
}

fn report(incident: Option<PageWaitRecordedIncident>) -> Report {
    let mut report: Report =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.report.json")).unwrap();
    report.page_waits = Some(PageWaitReport {
        source_state: Some(SourceState::Partial),
        source_reason: Some("Current page visibility is partial.".into()),
        title: culprit_core::t!("view.page_waits.title"),
        summary: culprit_core::t!("view.page_waits.summary.empty"),
        recorded_incident: incident,
        limitations: vec![culprit_core::t!("view.page_waits.limit.ownership")],
        entries: vec![],
    });
    report
}

fn compact(report: &Report) -> Value {
    crate::waits_view::report(report)["page_waits"].clone()
}

fn task_report(sql: DefinitionPayload) -> Report {
    let original = incident(sql);
    let mut report = report(None);
    let view = report.page_waits.as_mut().unwrap();
    view.entries.push(PageWaitExplanation {
        observation_key: "later-task-reference".into(),
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
        explanation: "The later task waited on its recorded resource.".into(),
        investigation: "Keep current state separate from earlier context.".into(),
        verify: "Compare the same native evidence.".into(),
    });
    report
}

#[test]
fn zero_current_waits_retain_only_the_recorded_context_without_private_receipts_or_unredacted_sql() {
    let original = incident(DefinitionPayload::sql(
        "SELECT recorded_waiter_payload FROM dbo.orders WHERE customer=N'private-literal-canary'",
        Engine::Mssql,
    ));
    let report = report(Some(original.clone()));
    let value = compact(&report);
    assert_eq!(value["source_state"], "partial");
    assert_eq!(value["entries_retained"], 0);
    assert_eq!(value["entries"], json!([]));
    let recorded = &value["recorded_incident"];
    assert_eq!(recorded["historical_context"], true);
    assert_eq!(recorded["qualification"], culprit_core::t!("view.page_waits.recorded.qualification"));
    assert_eq!(recorded["observed_at"], original.observed_at);
    assert_eq!(recorded["request"], json!(original.request));
    assert_eq!(recorded["resource"], json!(original.resource));
    assert_eq!(recorded["client"], json!(original.client));
    assert_eq!(recorded["statement"]["availability"], "available");
    assert!(recorded["statement"]["preview"].as_str().unwrap().contains("recorded_waiter_payload"));
    let encoded = serde_json::to_string(&value).unwrap();
    for private in [
        "private-literal-canary",
        "private-original-capture-canary",
        original.original.manifest_sha256.as_str(),
        original.original.source_scope_id.as_str(),
        original.original.batch_id.as_str(),
        original.original.record_id.as_str(),
        original.original.payload_sha256.as_str(),
    ] {
        assert!(!encoded.contains(private), "{private}");
    }
    for field in ["original", "capture", "payload_sha256", "native_key", "current_state", "owner", "target"] {
        assert!(recorded.get(field).is_none(), "{field}");
    }
    for field in ["text", "sha256", "redaction_version"] {
        assert!(recorded["statement"].get(field).is_none(), "{field}");
    }
}

#[test]
fn every_original_statement_unavailability_reason_survives_without_a_preview_or_replacement_text() {
    for reason in [
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let report = report(Some(incident(DefinitionPayload::Unavailable { reason })));
        let value = compact(&report);
        assert_eq!(value["entries_retained"], 0);
        let statement = &value["recorded_incident"]["statement"];
        assert_eq!(statement, &json!({"availability":"unavailable","reason":reason}));
        assert!(statement.get("preview").is_none());
        assert!(statement.get("preview_truncated").is_none());
    }
}

#[test]
fn recorded_statement_previews_bound_unicode_without_exporting_the_remaining_body() {
    for characters in [511_usize, 512, 513, 100_000] {
        let text = format!("SELECT [{}]", "界🦀".chars().cycle().take(characters - 9).collect::<String>());
        assert_eq!(text.chars().count(), characters);
        let original = incident(DefinitionPayload::sql(&text, Engine::Mssql));
        let report = report(Some(original));
        let value = compact(&report);
        let statement = &value["recorded_incident"]["statement"];
        let preview = statement["preview"].as_str().unwrap();
        assert_eq!(preview.chars().count(), characters.min(512));
        assert!(text.starts_with(preview));
        assert_eq!(statement["preview_truncated"], characters > 512);
        if characters > 512 {
            assert!(!preview.contains(']'));
            assert!(!serde_json::to_string(&value).unwrap().contains(&text));
        } else {
            assert_eq!(preview, text);
        }
    }
}

#[test]
fn missing_original_context_does_not_become_a_current_request_or_an_invented_recorded_incident() {
    let report = report(None);
    let value = compact(&report);
    assert!(value["recorded_incident"].is_null());
    assert_eq!(value["entries_retained"], 0);
    assert_eq!(value["entries"], json!([]));
    assert_eq!(value["summary"], culprit_core::t!("view.page_waits.summary.empty"));
}

#[test]
fn task_request_context_has_an_earlier_clock_and_bounded_sql_without_receipts_or_a_wait_resource() {
    let text = format!("SELECT [{}] FROM private_tail", "界".repeat(600));
    let report = task_report(DefinitionPayload::sql(&text, Engine::Mssql));
    let original = report.page_waits.as_ref().unwrap().entries[0].recorded_request.as_ref().unwrap();
    let value = compact(&report);
    assert!(value["recorded_incident"].is_null());
    let entry = &value["entries"][0];
    assert_eq!(entry["resource"]["native"]["id"], "5:1:84");
    assert_eq!(entry["task"]["execution_context_id"], 1);
    let context = &entry["recorded_request"];
    assert_eq!(context["historical_context"], true);
    assert_eq!(context["observed_at"], original.observed_at);
    assert_eq!(context["qualification"], culprit_core::t!("view.page_waits.recorded_request.qualification"));
    assert_eq!(context["request"], json!(original.request));
    assert_eq!(context["client"], json!(original.client));
    assert_eq!(context["statement"]["preview"].as_str().unwrap().chars().count(), 512);
    assert_eq!(context["statement"]["preview_truncated"], true);
    for field in ["resource", "original", "capture", "current_state", "owner", "target"] {
        assert!(context.get(field).is_none(), "{field}");
    }
    let encoded = serde_json::to_string(&value).unwrap();
    for private in [
        "private_tail",
        "private-original-capture-canary",
        original.original.manifest_sha256.as_str(),
        original.original.source_scope_id.as_str(),
        original.original.batch_id.as_str(),
        original.original.record_id.as_str(),
        original.original.payload_sha256.as_str(),
    ] {
        assert!(!encoded.contains(private), "{private}");
    }
}

#[test]
fn every_earlier_task_statement_reason_stays_explicit_without_a_replacement_sql_payload() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let report = task_report(DefinitionPayload::Unavailable { reason });
        let value = compact(&report);
        assert_eq!(
            value["entries"][0]["recorded_request"]["statement"],
            json!({"availability":"unavailable","reason":reason})
        );
    }
}

#[test]
fn an_unbound_task_request_does_not_synthesize_an_earlier_context_or_discard_the_wait() {
    let mut report = task_report(DefinitionPayload::sql("SELECT earlier_marker", Engine::Mssql));
    report.page_waits.as_mut().unwrap().entries[0].recorded_request = None;
    let value = compact(&report);
    assert!(value["entries"][0]["recorded_request"].is_null());
    assert_eq!(value["entries"][0]["resource"]["native"]["id"], "5:1:84");
    assert_eq!(value["entries"][0]["task"]["execution_context_id"], 1);
    assert!(!serde_json::to_string(&value).unwrap().contains("earlier_marker"));
}
