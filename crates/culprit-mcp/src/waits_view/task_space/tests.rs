use super::*;
use culprit_core::{Engine, SourceState, TaskAllocationActivity, TaskSpaceReport, telemetry::*};

fn current(statement: DefinitionPayload) -> TaskSpaceCurrentRequest {
    TaskSpaceCurrentRequest {
        request: ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "original-login".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "original-request".into() }),
            transaction: None,
        },
        observed_from: "2020-01-01T12:00:00.3Z".into(),
        observed_until: "2020-01-01T12:00:00.7Z".into(),
        statement,
        client: Some(ActivityClient { application: Some("reported-client".into()), user: None, host: None }),
    }
}
fn explanation(current_request: Option<TaskSpaceCurrentRequest>) -> TaskSpaceExplanation {
    TaskSpaceExplanation {
        observation_key: "original-task-reference".into(),
        task: TaskSpaceIdentity { session_id: "42".into(), request_id: "0".into(), execution_context_id: "1".into() },
        activity: TaskAllocationActivity::InternalObjects,
        current_request,
        title: "Native task".into(),
        explanation: "Allocation counters remain separate.".into(),
        investigation: "Inspect original evidence.".into(),
        verify: "Measure the tested change.".into(),
    }
}

#[test]
fn compact_current_request_keeps_its_interval_and_bounded_redacted_sql_without_private_payload_metadata() {
    let raw = format!("SELECT {} FROM dbo.orders WHERE note=N'current-mcp-secret-canary'", "列".repeat(700));
    let context = current(DefinitionPayload::sql(&raw, Engine::Mssql));
    let value = entry(&explanation(Some(context.clone())));
    let current = &value["current_request"];
    assert_eq!(current["request"], json!(context.request));
    assert_eq!(current["request_check_bounds"], json!({"from":context.observed_from,"until":context.observed_until}));
    assert_eq!(current["client"], json!(context.client));
    assert_eq!(current["statement"]["preview"].as_str().unwrap().chars().count(), 512);
    assert_eq!(current["statement"]["preview_truncated"], true);
    assert_eq!(current["qualification"], culprit_core::t!("view.task_space.current_request.qualification"));
    for field in ["text", "sha256", "redaction_version"] {
        assert!(current["statement"].get(field).is_none());
    }
    for field in [
        "observed_at",
        "task_counter_time",
        "allocator",
        "cause",
        "module",
        "statement_identity",
        "plan_identity",
        "receipt",
    ] {
        assert!(current.get(field).is_none(), "{field}");
    }
    assert!(!value.to_string().contains("current-mcp-secret-canary"));
    assert!(entry(&explanation(None))["current_request"].is_null());
}

#[test]
fn compact_unavailable_current_sql_preserves_every_reason_without_an_alternate_body() {
    for reason in [
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let value = entry(&explanation(Some(current(DefinitionPayload::Unavailable { reason }))));
        assert_eq!(value["current_request"]["statement"], json!({"availability":"unavailable","reason":reason}));
    }
}

#[test]
fn regular_waits_uses_the_same_projection_and_its_existing_entry_limit() {
    let mut report: culprit_core::Report =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.report.json")).unwrap();
    report.task_space = Some(TaskSpaceReport {
        source_state: Some(SourceState::Partial),
        source_reason: None,
        title: "Native tasks".into(),
        summary: "Bounded observations".into(),
        limitations: vec![],
        entries: (0..super::super::PARTICIPANT_ENTRIES + 1)
            .map(|_| {
                explanation(Some(current(DefinitionPayload::sql(
                    "SELECT canonical_task_context WHERE id=9264",
                    Engine::Mssql,
                ))))
            })
            .collect(),
    });
    let value = crate::waits_view::report(&report)["task_space"].clone();
    assert_eq!(value["entries_retained"], super::super::PARTICIPANT_ENTRIES + 1);
    assert_eq!(value["entries"].as_array().unwrap().len(), super::super::PARTICIPANT_ENTRIES);
    assert_eq!(value["entries"][0], entry(&report.task_space.as_ref().unwrap().entries[0]));
    assert!(!value.to_string().contains("9264"));
}
