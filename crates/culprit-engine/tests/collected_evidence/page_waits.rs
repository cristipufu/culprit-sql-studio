use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::page_wait_evidence_cases,
};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn observations(rows: &[TelemetryRecord]) -> Vec<&PageWaitObservation> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::PageWait(observation),
                ..
            }) = row
            else {
                panic!("page-wait fixture")
            };
            observation.as_ref()
        })
        .collect()
}

fn scan(source: &CollectionDescription, rows: &[TelemetryRecord]) -> Snapshot {
    let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let database = source.target.database().unwrap();
    scan.server.database = database.database.clone();
    scan.identity = Some(database.clone());
    let SourceObservation::Snapshot(native) = &source.observation else { panic!() };
    let coverage = &native.sources[0];
    scan.sources.retain(|source| source.source != coverage.source);
    scan.sources.push(SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    });
    scan.operations.page_waits = Some(PageWaitSnapshot {
        source: coverage.source.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        observations: observations(rows).into_iter().cloned().collect(),
    });
    scan
}

#[tokio::test]
async fn authenticated_page_targets_reuse_scan_explanations_and_reference_original_waiters_once() {
    for (source, rows) in page_wait_evidence_cases() {
        let expected = culprit_checks::analyze(scan(&source, &rows)).page_waits.unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::page_waits(&capture).unwrap();
        assert_eq!(result.report, expected);
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        for row in observations(&rows) {
            let entry = result.report.entries.iter().find(|entry| entry.observation_key == row.key().unwrap()).unwrap();
            assert_eq!(entry.request_key, row.request.key);
            assert_eq!(entry.resource, row.resource);
        }
        for row in &rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(result.context.records.iter().any(|actual| actual.row == expected));
        }
        let report = serde_json::to_string(&result.report).unwrap();
        assert!(!report.contains("INSERT INTO") && !report.contains("SELECT payload"));
    }
}

#[tokio::test]
async fn changed_unavailable_and_unverified_checks_keep_original_context_without_checked_statement_or_target() {
    let (source, rows) = page_wait_evidence_cases().remove(1);
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::page_waits(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    for resolution in
        [PageWaitResolution::ChangedRequest, PageWaitResolution::Unavailable, PageWaitResolution::UnverifiedSource]
    {
        let native = result
            .context
            .records
            .iter()
            .find_map(|row| match &row.row.record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::PageWait(observation),
                    ..
                }) if observation.resolution == resolution => Some(observation),
                _ => None,
            })
            .unwrap();
        assert!(native.request.key.request.is_some());
        assert!(native.request.statement.is_available(), "the originally observed SQL is retained as context");
        assert!(native.target.is_none() && native.statement_identity.is_none() && native.plan_identity.is_none());
        assert!(result.report.entries.iter().any(|entry| entry.observation_key == native.key().unwrap()));
    }
}

#[tokio::test]
async fn missing_page_rows_and_publication_are_store_gaps_without_rewriting_native_history() {
    let (source, rows) = page_wait_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::page_waits(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.report.entries.len(), 1);
    assert!(result.report.source_reason.is_some());
    assert_eq!(result.context.source.observation.snapshot().unwrap().sources[0].state, SourceState::Collected);
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::page_waits(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::NotPublished);
}

#[tokio::test]
async fn page_investigation_rejects_wrong_subject_dataset_family_database_and_observation_bounds() {
    for shape in ["shared-target", "dataset", "family", "database", "before-bounds", "after-bounds"] {
        let (mut source, mut rows) = page_wait_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(row), .. }) =
            &mut rows[0]
        else {
            panic!()
        };
        match shape {
            "shared-target" => {
                source.target = CollectionTarget::Shared(SharedResourceIdentity {
                    engine: culprit_core::Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-server"),
                    native: NativeIdentity::new("native-instance"),
                })
            }
            "dataset" => {
                let SourceObservation::Snapshot(native) = &mut source.observation else { panic!() };
                native.dataset = SnapshotDataset::Operational;
            }
            "family" => {}
            "database" => row.request.database = Some("another-db".into()),
            "before-bounds" => row.observed_from = "2020-01-01T11:59:59Z".into(),
            "after-bounds" => row.observed_until = "2020-01-01T12:00:02Z".into(),
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::page_waits(&capture).unwrap_err();
        let problem = if matches!(shape, "shared-target" | "dataset" | "family") {
            DeliveryProblem::UnsupportedSchema
        } else {
            DeliveryProblem::Integrity
        };
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(problem)), "{shape}");
    }
}

#[tokio::test]
async fn restricted_and_recovery_only_page_sql_follow_the_existing_payload_inventory_and_proof_rules() {
    let columns = (0..400).map(|column| format!("field_{column}")).collect::<Vec<_>>().join(", ");
    for statement in [
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
        DefinitionPayload::sql(
            &format!("SELECT {columns} FROM dbo.orders WHERE customer_id=9264"),
            culprit_core::Engine::Mssql,
        ),
    ] {
        let recovery_only = matches!(&statement, DefinitionPayload::Available { text, .. } if text.len() > 1024);
        let (source, mut rows) = page_wait_evidence_cases().remove(0);
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(row), .. }) =
            &mut rows[0]
        else {
            panic!()
        };
        row.request.statement = statement;
        let fixture = Fixture::new(source, rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::page_waits(&capture).unwrap();
        if recovery_only {
            assert_eq!(result.report.entries.len(), 1);
            assert_eq!(result.report.source_state, Some(SourceState::Partial));
            assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
        } else {
            assert!(capture.is_read_complete());
            assert_eq!(result.report.entries.len(), 2);
        }
    }
}

#[tokio::test]
async fn unresolved_waiters_from_another_scope_remain_unowned_original_context() {
    let (source, mut rows) = page_wait_evidence_cases().remove(1);
    let TelemetryRecord::Operational(record) = &mut rows[2] else { panic!() };
    let OperationalFact::PageWait(row) = &mut record.fact else { panic!() };
    row.resolution = PageWaitResolution::OutsideDatabase;
    row.request.scope = ActivityScopeBasis::OtherDatabase;
    row.request.database = Some("outside-db".into());
    row.resource.native.id = "99:1:42".into();
    row.request.wait.as_mut().unwrap().identified_resources = vec![row.resource.clone()];
    record.key = row.key().unwrap();
    let original = record.clone();
    let fixture = Fixture::new(source, rows).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::page_waits(&capture).unwrap();
    assert!(result.context.records.iter().any(|row| row.row.record == TelemetryRecord::Operational(original.clone())));
    assert!(result.report.entries.iter().any(|entry| entry.observation_key == original.key));
}
