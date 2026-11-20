use super::*;
use culprit_collector::analytical::test_support::{
    DatabasePropertiesCase, PerformanceCounterCase, compilation_definition_evidence_cases,
    database_properties_evidence, local_snapshot_evidence_cases, performance_counter_evidence,
};
use culprit_engine::LocalCaptureAnalysisKind;

#[tokio::test]
async fn every_shared_operational_report_reaches_the_existing_english_mcp_operation_without_native_work() {
    for case in local_snapshot_evidence_cases() {
        let kind: LocalCaptureAnalysisKind = serde_json::from_value(json!(case.analysis)).unwrap();
        let mut fixture = LocalCaptureFixture::snapshot(case.source, case.records).await;
        assert_eq!(fixture.manifest.status, CollectionStatus::Complete, "{}", case.analysis);
        let native = fixture.manifest.description.as_ref().unwrap().observation.snapshot().unwrap().clone();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            culprit_core::i18n::scope_async("fr", analyze(&server, &fixture.manifest.capture.observation_id, kind))
                .await;
        assert!(!error, "{}: {value}", case.analysis);
        let report = &value[case.analysis];
        assert!(report.is_object(), "{}", case.analysis);
        assert_eq!(value["analysis"], json!(kind));
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["native_rows"], native.records);
        assert_eq!(value["observed_rows"], native.records);
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["collected_from"], fixture.manifest.started_at);
        assert_eq!(value["collected_until"], json!(fixture.manifest.finished_at));
        assert_eq!(value["local_verification_bytes"], fixture.original_bytes());
        assert_eq!(value["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        assert_eq!(value["native_sources"].as_array().unwrap().len(), native.sources.len());
        let title_key = match kind {
            LocalCaptureAnalysisKind::DatabaseProperties => "view.database_properties.title",
            LocalCaptureAnalysisKind::Compilation => "view.compilation.title",
            LocalCaptureAnalysisKind::PerformanceCounters => "view.performance_counters.title",
            LocalCaptureAnalysisKind::TempdbMetadataMemory => "view.tempdb_metadata_memory.title",
            _ => "",
        };
        if !title_key.is_empty() {
            let expected = culprit_core::i18n::scope("en", || culprit_core::i18n::t(title_key, &[]));
            assert_eq!(report["title"], expected);
        }
        if let Some(entries) = report["entries"].as_array() {
            assert!(entries.len() <= 5);
            if let Some(available) = report["entries_available"].as_u64() {
                assert_eq!(report["entries_presented"], entries.len(), "{}", case.analysis);
                assert_eq!(report["entries_omitted"], available - entries.len() as u64, "{}", case.analysis);
            }
        }
        let text = value.to_string();
        for private in [
            "manifest_sha256",
            "source_sha256",
            "payload_sha256",
            "qualifiers",
            "fixture.invalid",
            "private-current-work-sql",
        ] {
            assert!(!text.contains(private), "{}: {private}", case.analysis);
        }
        assert!(text.len() <= 64 * 1024);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (error, again) = analyze(&server, &fixture.manifest.capture.observation_id, kind).await;
        assert!(!error);
        assert_eq!(again, value, "stable exact original capture references");
    }
}

#[tokio::test]
async fn compact_performance_and_property_reports_keep_native_unavailable_and_partial_states() {
    for (kind, (source, rows)) in [
        (LocalCaptureAnalysisKind::PerformanceCounters, performance_counter_evidence(PerformanceCounterCase::Denied)),
        (
            LocalCaptureAnalysisKind::PerformanceCounters,
            performance_counter_evidence(PerformanceCounterCase::NativePartial),
        ),
        (LocalCaptureAnalysisKind::DatabaseProperties, database_properties_evidence(DatabasePropertiesCase::Denied)),
        (
            LocalCaptureAnalysisKind::DatabaseProperties,
            database_properties_evidence(DatabasePropertiesCase::NativePartial),
        ),
    ] {
        let expected = source.observation.snapshot().unwrap().sources[0].state;
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) = analyze(&server, &fixture.manifest.capture.observation_id, kind).await;
        assert!(!error, "{value}");
        let name = serde_json::to_value(kind).unwrap();
        let report = &value[name.as_str().unwrap()];
        assert_eq!(report["source_state"], json!(expected));
        assert_eq!(report["read_complete"], true);
        assert_eq!(value["native_sources"][0]["state"], json!(expected));
        assert!(!report["limitations"].as_array().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn compiled_native_integer_counts_remain_exact_and_independent_from_bounded_report_details() {
    let mut case =
        compilation_definition_evidence_cases().into_iter().find(|case| case.name == "native_large_count").unwrap();
    let TelemetryRecord::Operational(template) = case.current.1.remove(0) else { panic!() };
    let rows = (0..7)
        .map(|index| {
            let mut row = template.clone();
            let OperationalFact::MssqlQueryCompilation(native) = &mut row.fact else { panic!() };
            native.identity.query_id = (index + 1).to_string();
            row.key = native.key().unwrap();
            TelemetryRecord::Operational(row)
        })
        .collect::<Vec<_>>();
    let SourceObservation::Snapshot(header) = &mut case.current.0.observation else { panic!() };
    header.records = 7;
    header.sources[0].records = 7;
    let mut fixture = LocalCaptureFixture::snapshot(case.current.0, rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let (error, value) =
        analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Compilation).await;
    assert!(!error, "{value}");
    let report = &value["compilation"];
    assert_eq!(value["native_rows"], 7);
    assert_eq!(value["observed_rows"], 7);
    assert_eq!(value["read_complete"], true);
    assert_eq!(report["entries_available"], 7);
    assert_eq!(report["entries_presented"], 5);
    assert_eq!(report["entries_omitted"], 2);
    assert_eq!(report["entries_limited"], true);
    assert!(report.to_string().contains("9007199254740993"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn compilation_retains_verified_bounded_sql_and_historical_incarnation_without_private_receipts() {
    for payload in ["available", "restricted", "unknown-namespace"] {
        let (mut source, mut rows) = culprit_collector::analytical::test_support::query_compilation_evidence();
        if payload == "unknown-namespace" {
            let CollectionTarget::Database(database) = &mut source.target else { panic!() };
            database.incarnation = None;
        }
        let namespace = query_store_database_namespace(&source.target, &source.partition);
        let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
        let OperationalFact::MssqlQueryCompilation(native) = &mut row.fact else { panic!() };
        native.origin = StatementOrigin::Application;
        native.statement = if payload == "restricted" {
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
        } else {
            let columns = (0..80).map(|index| format!("column_{index}")).collect::<Vec<_>>().join(",");
            DefinitionPayload::sql(
                &format!("SELECT {columns} FROM dbo.original_compile_input WHERE private_value='raw-literal-canary'"),
                Engine::Mssql,
            )
        };
        let query_id = native.identity.query_id.clone();
        let initial = native.identity.qualifiers["initial_compile_at"].clone();
        row.key = native.key().unwrap();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Compilation).await;
        assert!(!error, "{payload}: {value}");
        assert_eq!(value["historical_context"], true);
        let entry = &value["compilation"]["entries"][0];
        assert_eq!(entry["historical_native_identity"]["database_namespace"], json!(namespace));
        assert_eq!(entry["historical_native_identity"]["query_id"], query_id);
        assert_eq!(entry["historical_native_identity"]["context_settings_id"], "5");
        assert_eq!(entry["historical_native_identity"]["initial_compile_at"], initial);
        assert_eq!(entry["statement"]["binding"], "captured");
        assert_eq!(entry["statement"]["origin"], json!(StatementOrigin::Application));
        assert!(entry["statement"].get("definition").is_none());
        if payload == "restricted" {
            assert!(entry["statement"]["preview"].is_null());
            assert_eq!(entry["statement"]["unavailable"], json!(PayloadUnavailable::Restricted));
            assert!(entry["statement"]["unavailable_explanation"].is_string());
        } else {
            assert!(entry["statement"]["preview"].as_str().unwrap().starts_with("SELECT"));
            assert_eq!(entry["statement"]["preview"].as_str().unwrap().chars().count(), 512);
            assert_eq!(entry["statement"]["preview_truncated"], true);
            assert!(entry["statement"]["unavailable"].is_null());
        }
        let text = value.to_string();
        assert!(!text.contains("raw-literal-canary"));
        assert!(!text.contains("payload_sha256") && !text.contains("manifest_sha256"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn failed_or_unconfirmed_completed_snapshots_never_fall_back_to_a_saved_derived_report() {
    for failure in ["tampered", "unconfirmed"] {
        let (source, rows) = performance_counter_evidence(PerformanceCounterCase::Exact);
        let shutdown = if failure == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent };
        let mut fixture = LocalCaptureFixture::with_shutdown(source, rows, shutdown).await;
        if failure == "tampered" {
            let path = fixture.capture_directory.join(&fixture.manifest.files[0].file);
            let before = std::fs::read_to_string(&path).unwrap();
            let after = before.replace("9007199254740993", "9007199254740994");
            assert_ne!(before, after);
            std::fs::write(path, after).unwrap();
        }
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::PerformanceCounters)
                .await;
        assert!(error, "{failure}: {value}");
        assert!(value.get("performance_counters").is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
