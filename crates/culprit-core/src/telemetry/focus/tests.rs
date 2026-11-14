use super::*;

fn focus() -> ActivityWaitFocus {
    let mut original = super::super::activity::tests::request("42");
    let resource = ObservedWaitResource { kind: WaitResourceKind::MemoryPage, native: NativeIdentity::new("7:1:4144") };
    let statement_identity = NativeIdentity::new("native-statement").qualified("offset", "2");
    original.statement_identity = Some(statement_identity.clone());
    original.wait = Some(ActivityWait {
        native_type: Some("PAGELATCH_EX".into()),
        identified_resources: vec![resource.clone()],
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
    });
    let capture = CaptureIdentity {
        observation_id: "baseline".into(),
        revision: Some(CaptureRevision {
            outbox_id: "focus-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    ActivityWaitFocus {
        parent: DiagnosticEvidenceRef {
            batch_id: batch_id(&capture.observation_id, TelemetryStream::ActivityObservations).unwrap(),
            capture,
            manifest_sha256: digest(b"manifest"),
            source_scope_id: digest(b"scope"),
            stream: TelemetryStream::ActivityObservations,
            native_key: NativeRecordIdentity::Activity(original.key.clone()),
            record_id: digest(b"record"),
            payload_sha256: digest(b"payload"),
            representation_version: REPRESENTATION_VERSION,
            row_ordinal: 0,
        },
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
        source: "native-activity".into(),
        epoch: "native-epoch".into(),
        original_observed_at: "2020-01-01T00:00:00Z".into(),
        original,
        statement_identity,
        resource,
    }
}

#[test]
fn one_original_row_and_native_token_are_required_without_interpreting_provider_components() {
    let focus = focus();
    assert!(focus.is_valid());
    assert_eq!(serde_json::from_str::<ActivityWaitFocus>(&serde_json::to_string(&focus).unwrap()).unwrap(), focus);
    for field in ["parent-key", "statement", "wait", "resource", "scope", "database", "request"] {
        let mut changed = focus.clone();
        match field {
            "parent-key" => changed.original.key.session.generation = "reused".into(),
            "statement" => changed.statement_identity.id = "different-token".into(),
            "wait" => changed.original.wait.as_mut().unwrap().native_type = None,
            "resource" => changed.resource.native.id = "7:1:4145".into(),
            "scope" => changed.original.scope = ActivityScopeBasis::OtherDatabase,
            "database" => changed.original.database = Some("another-db".into()),
            _ => {
                changed.original.key.request = None;
                changed.original.request_counters = None;
            }
        }
        assert!(!changed.is_valid(), "{field}");
    }
}

#[test]
fn focus_identity_binds_original_receipt_and_client_without_a_warehouse_range_requirement() {
    let first = focus();
    let key = first.key().unwrap();
    for field in ["ordinal", "manifest", "source", "epoch", "clock", "sql"] {
        let mut changed = first.clone();
        match field {
            "ordinal" => changed.parent.row_ordinal += 1,
            "manifest" => changed.parent.manifest_sha256 = digest(b"another manifest"),
            "source" => changed.source = "another native source".into(),
            "epoch" => changed.epoch = "another source incarnation".into(),
            "clock" => changed.original_observed_at = "2020-01-01T00:00:01Z".into(),
            _ => changed.original.statement = DefinitionPayload::sql("SELECT 1", crate::Engine::Mssql),
        }
        assert!(changed.is_valid(), "structural validity cannot authenticate modified {field}");
        assert_ne!(changed.key().unwrap(), key, "exact parent resolution must bind {field}");
    }
}

#[test]
fn source_mismatch_or_disappeared_wait_never_authorizes_page_lookup() {
    let mut result = ActivityWaitFocusResult {
        focus: focus(),
        observed_from: "2020-01-01T00:00:01Z".into(),
        observed_until: "2020-01-01T00:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
    };
    assert!(result.permits_page_lookup());
    for wait in [PageWaitRelation::NoLongerObserved, PageWaitRelation::Different, PageWaitRelation::Unqualified] {
        result.wait = wait;
        assert!(result.is_valid());
        assert!(!result.permits_page_lookup());
    }
    for source in [FocusSourceState::Changed, FocusSourceState::Unverified] {
        result.source = source;
        result.request = PageWaitRequestState::Unverified;
        result.wait = PageWaitRelation::Unqualified;
        assert!(result.is_valid());
        assert!(!result.permits_page_lookup());
        result.request = PageWaitRequestState::SameStatement;
        assert!(!result.is_valid());
    }
}

#[test]
fn later_lookup_bounds_cannot_move_the_original_native_observation_forward() {
    let mut result = ActivityWaitFocusResult {
        focus: focus(),
        observed_from: "2019-12-31T23:59:59Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    assert!(!result.is_valid());
    result.observed_from = "2020-01-01T00:00:00Z".into();
    assert!(result.is_valid());
    assert_eq!(result.focus.original_observed_at, "2020-01-01T00:00:00Z");
    result.observed_until = "2019-12-31T23:59:59Z".into();
    assert!(!result.is_valid());
}
