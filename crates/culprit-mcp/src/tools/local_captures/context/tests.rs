use super::*;
use culprit_core::{Engine, telemetry::*};

struct Original {
    read: ActivityCensusRead,
    reference: DiagnosticEvidenceRef,
}
impl Original {
    fn new(negative: bool) -> Self {
        let mut read: ActivityCensusRead = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/session-census.native.json"
        )))
        .unwrap();
        let row = &mut read.observations[0];
        row.statement = DefinitionPayload::sql(
            "SELECT original_column FROM original_table WHERE secret='private-literal-canary'",
            Engine::Mssql,
        );
        let context = row.session_context.as_deref_mut().unwrap();
        let SessionNativeContext::Mssql(native) = context.native.as_deref_mut().unwrap() else {
            panic!("MSSQL test context")
        };
        if negative {
            context.lifecycle = SessionLifecycle::PoolResetPrelogin;
            context.login_age_ms = None;
            native.native_state = "Dormant".into();
            native.login_established = false;
        } else {
            row.idle = false;
            row.key.request =
                Some(NativeActivityIdentity { id: "0".into(), generation: "native-request-generation".into() });
            row.request_counters = Some(ActivityCounters::default());
            context.lifecycle = SessionLifecycle::Running;
            native.native_state = "Running".into();
            native.has_current_request = true;
        }
        read.source.partition = SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("fixture-machine"),
                native: NativeIdentity::new("fixture-member"),
            }),
            epoch: Some("native-source-generation".into()),
        };
        let capture = CaptureIdentity {
            observation_id: "original-activity".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-context-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let exported =
            ExportedRecord::new(&read.source, &capture, TelemetryRecord::from(read.observations[0].clone())).unwrap();
        let reference =
            DiagnosticEvidenceRef::from_export(&read.source, &digest(b"original manifest"), &exported, 0).unwrap();
        Self { read, reference }
    }
    fn frame(&self) -> ActivityFrameEvidence<'_> {
        ActivityFrameEvidence::new(
            &self.read.source,
            &[ActivityFrameRow { observation: &self.read.observations[0], reference: Some(&self.reference) }],
            true,
        )
        .unwrap()
    }
}

#[test]
fn exact_original_receipts_preserve_their_own_context_but_reject_same_key_substitutions() {
    let original = Original::new(false);
    let frame = original.frame();
    let value = referenced_participant(&frame, &original.reference).unwrap();
    assert_eq!(value["actor"], json!(original.read.observations[0].key));
    assert_eq!(value["observed_at"], frame.native().at);
    assert!(
        value["statement"]["preview"]
            .as_str()
            .is_some_and(|sql| sql.contains("original_column") && !sql.contains("private-literal-canary"))
    );
    for field in ["manifest", "capture", "ordinal", "payload"] {
        let mut substituted = original.reference.clone();
        match field {
            "manifest" => substituted.manifest_sha256 = digest(b"another manifest"),
            "capture" => {
                let mut capture = original.reference.capture.clone();
                capture.observation_id = "another-capture".into();
                let exported = ExportedRecord::new(
                    frame.source(),
                    &capture,
                    TelemetryRecord::from(original.read.observations[0].clone()),
                )
                .unwrap();
                substituted = DiagnosticEvidenceRef::from_export(
                    frame.source(),
                    &original.reference.manifest_sha256,
                    &exported,
                    0,
                )
                .unwrap();
                substituted.validate_canonical_record(frame.source(), &exported.record).unwrap();
            }
            "ordinal" => substituted.row_ordinal = 1,
            _ => substituted.payload_sha256 = digest(b"another payload"),
        }
        assert!(substituted.is_valid(), "{field}: the foreign receipt is structurally valid");
        assert!(referenced_participant(&frame, &substituted).is_err(), "{field}");
    }
}

#[test]
fn a_known_negative_original_is_raw_context_without_current_statement_or_graph_authority() {
    let original = Original::new(true);
    let frame = original.frame();
    let value = referenced_participant(&frame, &original.reference).unwrap();
    assert_eq!(value["actor"], json!(original.read.observations[0].key));
    assert_eq!(value["client"], json!(original.read.observations[0].client));
    assert_eq!(value["session_generation_disqualified"], true);
    assert!(value["statement"].is_null());
    assert!(
        graph_participant(&frame, &original.read.observations[0].key, Some(SessionStatementContext::CurrentRequest))
            .is_err()
    );
}

#[test]
fn graph_context_requires_the_exact_same_frame_request_and_keeps_prior_sql_independent() {
    let original = Original::new(false);
    let frame = original.frame();
    let key = &original.read.observations[0].key;
    let value = graph_participant(&frame, key, Some(SessionStatementContext::CurrentRequest)).unwrap();
    assert!(value["statement"].is_object());
    let prior = graph_participant(&frame, key, Some(SessionStatementContext::PriorSessionBatch)).unwrap();
    assert!(prior["statement"].is_null());
    for field in ["generation", "request"] {
        let mut changed = key.clone();
        if field == "generation" {
            changed.session.generation = "another-session-generation".into();
        } else {
            changed.request.as_mut().unwrap().id = "another-request".into();
        }
        assert!(graph_participant(&frame, &changed, Some(SessionStatementContext::CurrentRequest)).is_err());
    }
}
