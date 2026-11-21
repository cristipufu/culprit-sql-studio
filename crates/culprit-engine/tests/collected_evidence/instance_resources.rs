use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, InstanceResourceCase},
};
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
async fn original_processor_cpu_source_replays_separately_from_the_instance_resource_point_without_requery() {
    use test_support::{ProcessCpuCase, instance_resource_cpu_evidence};
    for case in ProcessCpuCase::ALL {
        let (source, rows) = instance_resource_cpu_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
        let cpu = report.process_cpu.as_ref().unwrap();
        assert_eq!(report.native_source.as_ref().unwrap().source, INSTANCE_RESOURCES_SOURCE);
        assert_eq!(cpu.native_source.as_ref().unwrap().source, culprit_core::source_id::PROCESSOR_TOPOLOGY);
        if case == ProcessCpuCase::Denied {
            assert_eq!(cpu.source_state, SourceState::Unavailable);
            assert!(cpu.fields.is_empty() && cpu.observed_at.is_none());
        } else {
            assert_eq!(cpu.observed_at.as_deref(), Some("2020-01-01T12:00:00.5Z"));
            assert_ne!(cpu.observed_at, report.observed_at);
            assert_eq!(
                cpu.source_state,
                if case == ProcessCpuCase::Partial { SourceState::Partial } else { SourceState::Collected }
            );
            let expected = match case {
                ProcessCpuCase::Exact | ProcessCpuCase::Partial => Some("9007199254740993"),
                ProcessCpuCase::Zero => Some("0"),
                ProcessCpuCase::Null => None,
                _ => unreachable!(),
            };
            assert_eq!(cpu.fields.iter().find(|field| field.key == "user_time_ms").unwrap().value.as_deref(), expected);
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_cpu_body_preserves_declared_original_source_and_never_borrows_the_memory_point_clock() {
    let (source, rows) = test_support::instance_resource_cpu_evidence(test_support::ProcessCpuCase::Exact);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 4);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
    let cpu = report.process_cpu.as_ref().unwrap();
    assert_eq!(cpu.source_state, SourceState::Partial);
    assert_eq!(cpu.native_source.as_ref().unwrap().records, 4);
    assert_eq!(cpu.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert!(cpu.observed_at.is_none() && cpu.fields.is_empty());
    assert!(report.observed_at.is_some());
}

#[tokio::test]
async fn instance_points_replay_exact_original_scope_populations_flags_and_pure_meanings_without_database_work() {
    for case in InstanceResourceCase::ALL {
        let (source, rows) = test_support::instance_resource_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
        let original = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let read = InstanceResourceRead::from_source(&source, &original, true).unwrap();
        let expected = culprit_checks::instance_resources::explain(&read).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(*analysis.report, expected);
        let context = analysis.report.context.as_ref().unwrap().native().unwrap();
        assert!(matches!(context.target, CollectionTarget::Shared(_)));
        assert_eq!(context.partition, source.partition);
        assert!(context.target.database().is_none());
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn absent_instance_body_preserves_original_native_row_count_and_never_hydrates_or_fabricates_zero() {
    let (source, rows) = test_support::instance_resource_evidence(InstanceResourceCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().records, 1);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!(analysis.report.read_complete, Some(false));
    assert!(analysis.report.groups.is_empty() && analysis.report.observed_at.is_none());
    assert_eq!(analysis.report.native_populations_complete, None);
    assert_eq!(fixture.recovery_object_reads(), opened);
}
