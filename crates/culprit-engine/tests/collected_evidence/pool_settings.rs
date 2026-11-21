use super::fixture::Fixture;
use super::*;
use culprit_collector::analytical::evidence::EvidenceFamily;
use culprit_collector::analytical::test_support::{PoolSettingsCase, pool_settings_evidence};
use culprit_engine::collected_resources;
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::SettingObservations];
#[tokio::test]
async fn recorded_pool_settings_reuse_original_projection_scope_clocks_nulls_and_pure_meaning() {
    for case in PoolSettingsCase::ALL {
        let (source, rows) = pool_settings_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::pool_settings(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        let observations = rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Setting(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::pool_settings::explain(culprit_checks::pool_settings::PoolSettingsEvidence {
            source: &source,
            observations: &observations,
            read_complete: true,
        })
        .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.report.native_source.as_ref(), Some(&source.observation.snapshot().unwrap().sources[0]));
        assert_eq!(analysis.report.target.as_ref(), Some(&source.target));
        assert_eq!(
            analysis.report.elastic_pool.is_some(),
            !matches!(
                case,
                PoolSettingsCase::SingleDatabase
                    | PoolSettingsCase::MembershipDenied
                    | PoolSettingsCase::MembershipUnavailable
            )
        );
        assert_eq!(analysis.report.member_database.as_deref(), Some("recorded-member"));
        assert_eq!(fixture.recovery_object_reads(), opened);
        if case == PoolSettingsCase::Exact {
            assert_eq!(analysis.report.fields[0].native_value.as_deref(), Some("0"));
            assert_eq!(analysis.report.fields[3].native_value, None);
        }
        if matches!(case, PoolSettingsCase::Denied | PoolSettingsCase::MissingLayout) {
            assert!(analysis.report.fields.is_empty());
            assert_eq!(analysis.report.source_state, culprit_core::SourceState::Unavailable);
        }
        let value = serde_json::to_string(&analysis.report).unwrap();
        if matches!(source.target, CollectionTarget::Shared(_)) {
            assert!(!value.contains("11111111-1111-1111-1111-111111111111"));
        }
        assert!(!value.contains("qualifiers\":{\"logical_database_guid"));
    }
}
#[tokio::test]
async fn absent_archive_row_keeps_original_pool_population_and_current_point_without_a_substitute_or_zero() {
    let (source, rows) = pool_settings_evidence(PoolSettingsCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::SettingObservations);
    let capture = fixture.read_families(FAMILY).await;
    let report = collected_resources::pool_settings(&capture).unwrap().report;
    assert_eq!(report.native_rows, 6);
    assert_eq!(report.observed_rows, 5);
    assert!(!report.read_complete);
    assert_eq!(report.source_state, culprit_core::SourceState::Partial);
    assert!(!report.fields.iter().any(|field| field.key == "min_cpu"));
    assert_eq!(report.observed_at.as_deref(), Some("2026-11-01T00:00:02Z"));
    assert_eq!(capture.source(), &source);
}
#[tokio::test]
async fn unrelated_operational_capture_cannot_supply_a_pool_settings_header_or_body() {
    let (source, rows) = culprit_collector::analytical::test_support::database_properties_evidence(
        culprit_collector::analytical::test_support::DatabasePropertiesCase::Exact,
    );
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::pool_settings(&capture).is_err());
}
