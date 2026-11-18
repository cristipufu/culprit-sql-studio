use super::*;

fn captured() -> CapturedOperationalRead {
    let read = read();
    let identity = CaptureIdentity {
        observation_id: "original-component-capture".into(),
        revision: Some(CaptureRevision {
            outbox_id: "owned-component-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(b"original manifest"),
        source_scope_id: read.description.captured_range().unwrap().scope_id,
    };
    let positions = read
        .observations
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported =
                ExportedRecord::new(&read.description, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
            CapturedSnapshotPosition::from_export(&exported, ordinal as u32).unwrap()
        })
        .collect();
    CapturedOperationalRead::captured(*read.description, capture, positions, read.observations, true).unwrap()
}
fn scan(read: CapturedOperationalRead) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    let database = read.description.target.database().unwrap().clone();
    snapshot.server.database = database.database.clone();
    snapshot.bind_identity(database);
    snapshot.operations.hyperscale_log_components = Some(Box::new(read));
    snapshot
}
#[test]
fn captured_original_reuses_one_interpreter_with_exact_receipts_and_independent_read_gaps() {
    let expected = explain(&read()).unwrap();
    let read = captured();
    assert_eq!(
        serde_json::to_value(explain_captured(&read).unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let original = read.clone();
    let mut scan = scan(read);
    assert!(explain_snapshot(&scan).unwrap().is_some());
    let captured = scan.operations.hyperscale_log_components.as_mut().unwrap();
    captured.observations.clear();
    captured.origin.positions.clear();
    captured.read_complete = false;
    let report = explain_snapshot(&scan).unwrap().unwrap();
    assert_eq!(report.native_rows, 1);
    assert_eq!(report.observed_rows, 0);
    assert!(!report.read_complete);
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(scan.operations.hyperscale_log_components.as_ref().unwrap().origin.capture, original.origin.capture);
}
#[test]
fn attaching_historical_components_requires_known_native_database_incarnation_not_labels_or_endpoints() {
    for change in ["alias", "unknown", "foreign", "missing", "engine", "identity-label", "body"] {
        let mut snapshot = scan(captured());
        match change {
            "alias" => {
                let identity = snapshot.identity.as_mut().unwrap();
                identity.database = "renamed".into();
                identity.location = culprit_core::DatabaseLocation::Server {
                    host: "another-route.invalid".into(),
                    port: 1444,
                    instance: None,
                };
                snapshot.server.database = "renamed".into();
            }
            "unknown" => snapshot.identity.as_mut().unwrap().incarnation = None,
            "foreign" => snapshot.identity.as_mut().unwrap().incarnation = Some("foreign-database".into()),
            "missing" => snapshot.identity = None,
            "engine" => snapshot.identity.as_mut().unwrap().engine = culprit_core::Engine::Postgres,
            "identity-label" => snapshot.identity.as_mut().unwrap().database = "wrong".into(),
            "body" => {
                let OperationalFact::HyperscaleLogComponent(point) =
                    &mut snapshot.operations.hyperscale_log_components.as_mut().unwrap().observations[0].fact
                else {
                    panic!()
                };
                point.role = 255;
            }
            _ => unreachable!(),
        }
        assert_eq!(explain_snapshot(&snapshot).is_ok(), change == "alias", "{change}");
    }
}
