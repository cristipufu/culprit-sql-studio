use super::*;
use culprit_collector::{
    analytical::test_support::{LocalCaptureFixture, PoolSettingsCase, pool_settings_evidence},
    outbox::CompletedCaptureBudget,
};
use culprit_core::limits::OperationGuard;
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, LocalPoolSettings,
    OutboxCaptureReader,
};
async fn original(case: PoolSettingsCase) -> Box<LocalPoolSettings> {
    let (source, rows) = pool_settings_evidence(case);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::PoolSettings,
            operation: guard.handle(),
        })
        .await
        .unwrap();
    assert_eq!(fixture.manifest.source_queries, 0);
    let LocalCapturedAnalysis::PoolSettings(result) = result else { panic!() };
    result
}
#[tokio::test]
async fn canonical_original_to_cli_keeps_all_six_meanings_nullable_zero_and_source_qualifications() {
    for case in PoolSettingsCase::ALL {
        let original = original(case).await;
        let report = original.report();
        let text = pool_settings(report);
        let value = compact(report);
        assert!(text.contains(&report.summary));
        assert!(text.contains(&report.investigation));
        assert!(text.contains(&report.verify));
        for field in &report.fields {
            assert!(text.contains(&field.label));
            assert!(text.contains(&field.explanation));
        }
        assert_eq!(value["fields"].as_array().unwrap().len(), report.fields.len());
        assert_eq!(value["scope"], "pool");
        assert_eq!(value["member_database"], "recorded-member");
        assert!(text.contains("recorded-member") && text.contains("pool-logical-server"));
        if case == PoolSettingsCase::SingleDatabase {
            assert_eq!(value["target_scope"], "database");
            assert!(value["pool"].is_null());
            assert!(text.contains(&t!("view.pool_settings.summary.unpooled")));
        }
        for private in [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "manifest_sha256",
            "source_scope_id",
        ] {
            assert!(!text.contains(private));
            assert!(!value.to_string().contains(private));
        }
        if case == PoolSettingsCase::Exact {
            assert_eq!(value["fields"][0]["native_value"], "0");
            assert!(value["fields"][3]["value"].is_null());
        }
    }
}
#[tokio::test]
async fn cli_bounds_every_native_name_field_and_final_action_without_terminal_control_injection() {
    let original = original(PoolSettingsCase::Exact).await;
    let mut report = original.report().clone();
    report.elastic_pool.as_mut().unwrap().native.id = format!("pool\n\u{1b}[31m{}", "p".repeat(2000));
    report.fields[0].explanation = "x".repeat(3000);
    report.fields.extend(report.fields.clone());
    report.limitations = vec!["l".repeat(3000); 12];
    report.verify = format!("verify-{}", "v".repeat(3000));
    let text = pool_settings(&report);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\n\\u{1b}"));
    assert!(!text.contains(&"x".repeat(1025)));
    assert!(!text.contains(&"v".repeat(1025)));
    assert!(text.contains("verify-"));
    let value = compact(&report);
    assert_eq!(value["fields"].as_array().unwrap().len(), 6);
    assert_eq!(value["fields_available"], 12);
    assert_eq!(value["fields_limited"], true);
    assert_eq!(value["limitations"].as_array().unwrap().len(), 8);
}
