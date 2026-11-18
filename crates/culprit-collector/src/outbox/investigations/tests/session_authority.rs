use super::*;

fn peer(lifecycle: Option<SessionLifecycle>) -> ActivityObservation {
    let mut row = focus::page_activity();
    row.key.request.as_mut().unwrap().id = "1".into();
    row.session_context = lifecycle.map(|lifecycle| {
        Box::new(ActivitySessionContext {
            lifecycle,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        })
    });
    row
}

#[tokio::test]
async fn cold_canonical_originals_keep_raw_peers_but_refuse_positive_row_authority_when_a_peer_is_negative() {
    for lifecycle in [
        None,
        Some(SessionLifecycle::Unknown),
        Some(SessionLifecycle::Connecting),
        Some(SessionLifecycle::PoolResetPrelogin),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outbox");
        let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
        let capture = baseline(&mut files, vec![focus::page_activity(), peer(lifecycle)], true, 0);
        drop(files);
        let mut files = Files::open(&path).unwrap();
        let input = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        assert_eq!(input.rows.len(), 2);
        let selected = focus::focus_at(&input, 0);
        assert!(selected.is_valid(), "a positive singleton cannot reveal the recorded peer");
        let result = files.verify_focus_parent(selected.participant());
        let negative = matches!(lifecycle, Some(SessionLifecycle::Connecting | SessionLifecycle::PoolResetPrelogin));
        if negative {
            assert_eq!(CollectionStop::from_error(&result.unwrap_err()), CollectionStop::SourceChanged);
        } else {
            result.unwrap();
        }
        for original in &input.rows {
            let (source, row) = files.diagnostic_reference(&original.reference).unwrap();
            assert_eq!(source, input.source);
            assert_eq!(row, original.row, "authority refusal cannot quarantine or rewrite genuine bytes");
        }
        let retained = files.diagnostic_input(&capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
        assert_eq!(retained.rows.len(), 2);
        assert_eq!(retained.rows[0].reference, input.rows[0].reference);
        assert_eq!(retained.rows[1].row, input.rows[1].row);
    }
}

#[tokio::test]
async fn another_real_capture_cannot_lend_its_positive_peer_population_to_a_genuine_original_row() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut files = Files::create(&path, OutboxLimits::default()).unwrap();
    let refused = baseline(&mut files, vec![focus::page_activity(), peer(Some(SessionLifecycle::Connecting))], true, 0);
    let allowed = baseline(&mut files, vec![focus::page_activity(), peer(None)], true, 2);
    drop(files);
    let mut files = Files::open(&path).unwrap();
    let bad = files.diagnostic_input(&refused.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let good = files.diagnostic_input(&allowed.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).unwrap();
    let mut selected = focus::focus_at(&good, 0);
    files.verify_focus_parent(selected.participant()).unwrap();
    selected.participant.parent = bad.rows[0].reference.clone();
    selected.participant.original_observed_at = bad.source.observation.activity().unwrap().at.clone();
    let TelemetryRecord::Activity(original) = &bad.rows[0].row.record else { panic!() };
    assert!(selected.matches_parent(&bad.source, original, &bad.rows[0].reference));
    assert_eq!(
        CollectionStop::from_error(&files.verify_focus_parent(selected.participant()).unwrap_err()),
        CollectionStop::SourceChanged
    );
    assert!(files.diagnostic_reference(&bad.rows[0].reference).is_ok());
}
