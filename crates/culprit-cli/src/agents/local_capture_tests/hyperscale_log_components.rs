use super::*;
use culprit_collector::analytical::test_support::hyperscale_log_component_evidence_cases;
use culprit_engine::LocalCaptureAnalysisKind;

fn component_originals(count: usize) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let TelemetryRecord::Operational(template) = &rows[0] else { panic!() };
    let rows = (0..count)
        .map(|index| {
            let mut row = template.clone();
            let OperationalFact::HyperscaleLogComponent(point) = &mut row.fact else { panic!() };
            point.component_id = format!("{index:08x}-2222-3333-4444-555555555555");
            row.key = point.key().unwrap();
            TelemetryRecord::Operational(row)
        })
        .collect();
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = count as u32;
    header.sources[0] = StateSourceCoverage::collected(HYPERSCALE_LOG_COMPONENTS_SOURCE.into(), count as u32);
    (source, rows)
}
#[tokio::test]
async fn original_component_capture_reaches_real_mcp_action_in_english_with_bounded_details_and_no_raw_proof() {
    for (index, (source, rows)) in
        hyperscale_log_component_evidence_cases().into_iter().chain([component_originals(7)]).enumerate()
    {
        let coverage = source.observation.snapshot().unwrap().sources[0].clone();
        let from = source.observation.snapshot().unwrap().observed_from.clone();
        let until = source.observation.snapshot().unwrap().observed_until.clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let native_calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, native_calls.clone());
        let (error, result) = culprit_core::i18n::scope_async(
            "fr",
            analyze(
                &server,
                &fixture.manifest.capture.observation_id,
                LocalCaptureAnalysisKind::HyperscaleLogComponents,
            ),
        )
        .await;
        assert!(!error, "{index}: {result}");
        let view = &result["hyperscale_log_components"];
        assert_eq!(
            view["title"],
            culprit_core::i18n::scope("en", || culprit_core::t!("view.hyperscale_log_components.title"))
        );
        assert_eq!(view["native_rows"], coverage.records);
        assert_eq!(view["observed_rows"], coverage.records);
        assert_eq!(view["source_state"], json!(coverage.state));
        assert_eq!(view["native_source_state"], json!(coverage.state));
        assert_eq!(view["read_complete"], true);
        assert_eq!(view["original_read_bounds"], json!({"from":from,"until":until}));
        assert_eq!(view["entries_presented"], coverage.records.min(5));
        assert_eq!(view["entries_limited"], coverage.records > 5);
        assert_eq!(result["local_verification_bytes"], fixture.original_bytes());
        if index == 0 {
            assert_eq!(view["entries"][0]["current_max_log_rate_bps"], "9007199254740993");
            assert_eq!(view["entries"][0]["catchup_rate_bps"], "0");
            assert_eq!(view["entries"][0]["last_reported_time_without_offset"], "2026-11-18T12:00:00.003");
            assert!(view["entries"][1]["replica_id"].is_null());
        }
        let text = result.to_string();
        for private in [
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "fixture.invalid",
            "same-read-native-process-epoch",
            "native-hs-logical-database",
            "database_incarnation",
        ] {
            assert!(!text.contains(private), "{index}: {private}");
        }
        assert!(text.len() <= 64 * 1024);
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        let (error, again) = analyze(
            &server,
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::HyperscaleLogComponents,
        )
        .await;
        assert!(!error);
        assert_eq!(result, again, "original capture/position references remain stable");
    }
}
#[tokio::test]
async fn corrupt_original_component_values_and_unconfirmed_sources_never_return_derived_mcp_fallbacks() {
    for failure in ["corrupt", "unconfirmed"] {
        let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
        let shutdown = if failure == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent };
        let mut fixture = LocalCaptureFixture::with_shutdown(source, rows, shutdown).await;
        if failure == "corrupt" {
            let path = fixture.capture_directory.join(&fixture.manifest.files[0].file);
            let before = std::fs::read_to_string(&path).unwrap();
            let changed = before.replace("9007199254740993", "9007199254740994");
            assert_ne!(before, changed);
            assert_eq!(before.len(), changed.len());
            std::fs::write(path, changed).unwrap();
        }
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, result) = analyze(
            &server,
            &fixture.manifest.capture.observation_id,
            LocalCaptureAnalysisKind::HyperscaleLogComponents,
        )
        .await;
        assert!(error, "{failure}");
        assert!(result.get("hyperscale_log_components").is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
