use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::session_memory_evidence_cases,
};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn observations(rows: &[TelemetryRecord]) -> Vec<&SessionMemoryObservation> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::SessionMemory(observation),
                ..
            }) = row
            else {
                panic!("session allocation fixture")
            };
            observation.as_ref()
        })
        .collect()
}

fn scan(source: &CollectionDescription, rows: &[TelemetryRecord]) -> Snapshot {
    let mut scan =
        serde_json::from_str::<culprit_core::Report>(include_str!("../../../../fixtures/mysql-demo.report.json"))
            .unwrap()
            .snapshot;
    let CollectionTarget::Shared(resource) = &source.target else { panic!("shared native instance") };
    let SourceObservation::Snapshot(native) = &source.observation else { panic!("native read bounds") };
    let coverage = &native.sources[0];
    scan.sources.retain(|source| source.source != coverage.source);
    scan.sources.push(SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    });
    scan.operations.session_memory = Some(SessionMemorySnapshot {
        source: coverage.source.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        resource: resource.clone(),
        observations: observations(rows).into_iter().cloned().collect(),
    });
    scan
}

#[tokio::test]
async fn authenticated_session_allocations_reuse_scan_views_without_an_allocating_query_or_database() {
    for (source, rows) in session_memory_evidence_cases() {
        let expected = culprit_checks::analyze(scan(&source, &rows)).session_memory.unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::session_memory(&capture).unwrap();
        assert_eq!(result.report, expected);
        assert_eq!(result.context.source, source);
        assert!(result.context.source.target.database().is_none());
        assert_eq!(result.context.records.len(), rows.len());
        for row in &rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(result.context.records.iter().any(|actual| actual.row == expected));
        }
        let report = serde_json::to_string(&result.report).unwrap();
        assert!(!report.contains("context-only-other-db") && !report.contains("context-only-native-digest"));
    }
}

#[tokio::test]
async fn native_allocation_and_current_event_clocks_stay_distinct_and_unknown_context_is_not_filled() {
    let (source, rows) = session_memory_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::session_memory(&capture).unwrap();
    let actual = result.context.records.iter().map(|row| &row.row.record).cloned().collect::<Vec<_>>();
    let native = observations(&actual);
    let associated = native.iter().find(|row| row.current_context.is_some()).unwrap();
    assert_ne!(associated.observed_at, associated.current_context.as_ref().unwrap().observed_at);
    assert_eq!(associated.current_bytes.as_deref(), Some("18446744073709551615"));
    let unassociated = native.iter().find(|row| row.current_context.is_none()).unwrap();
    assert_eq!(unassociated.current_bytes.as_deref(), Some("0"));
    let entry =
        result.report.entries.iter().find(|entry| entry.observation_key == unassociated.key().unwrap()).unwrap();
    assert!(entry.explanation.contains(&culprit_core::t!("view.session_memory.context.unavailable")));
}

#[tokio::test]
async fn archived_session_row_loss_does_not_rewrite_native_source_state_or_hide_its_context() {
    let (source, rows) = session_memory_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::session_memory(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.report.entries.len(), 1);
    assert!(result.report.source_reason.is_some());
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::session_memory(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::NotPublished);
}

#[tokio::test]
async fn conflicting_session_scope_family_and_independent_clock_bounds_are_rejected() {
    for shape in ["database-target", "pool-target", "dataset", "allocation-clock", "context-clock", "wrong-family"] {
        let (mut source, mut rows) = session_memory_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::SessionMemory(row), .. }) =
            &mut rows[0]
        else {
            panic!()
        };
        match shape {
            "database-target" => {
                source.target = culprit_core::DatabaseIdentity::server(
                    culprit_core::Engine::Mysql,
                    "context-only-other-db",
                    "fixture.invalid",
                    3306,
                    None,
                )
                .into()
            }
            "pool-target" => {
                let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
                resource.kind = SharedResourceKind::Pool;
            }
            "dataset" => {
                let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
                native.dataset = SnapshotDataset::Operational;
            }
            "allocation-clock" => row.observed_at = "2020-01-01T12:00:02Z".into(),
            "context-clock" => row.current_context.as_mut().unwrap().observed_at = "2020-01-01T12:00:02Z".into(),
            "wrong-family" => {}
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "wrong-family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::session_memory(&capture).unwrap_err();
        let problem = if matches!(shape, "allocation-clock" | "context-clock") {
            DeliveryProblem::Integrity
        } else {
            DeliveryProblem::UnsupportedSchema
        };
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(problem)), "{shape}");
    }
}
