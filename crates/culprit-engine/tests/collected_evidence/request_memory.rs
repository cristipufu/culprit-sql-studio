use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{request_memory_evidence, request_memory_evidence_cases},
};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn observations(rows: &[TelemetryRecord]) -> Vec<&ExecutionGrantObservation> {
    rows.iter()
        .map(|row| {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = row
            else {
                panic!("grant fixture")
            };
            grant.as_ref()
        })
        .collect()
}

fn native(source: &mut CollectionDescription) -> &mut StateSnapshotDescription {
    let SourceObservation::Snapshot(native) = &mut source.observation else { panic!("point fixture") };
    native
}

fn scan(source: &CollectionDescription, rows: &[TelemetryRecord]) -> Snapshot {
    let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    let database = source.target.database().unwrap();
    scan.server.database = database.database.clone();
    scan.identity = Some(database.clone());
    let SourceObservation::Snapshot(native) = &source.observation else { panic!("native fixture") };
    let coverage = &native.sources[0];
    scan.sources.retain(|source| source.source != coverage.source);
    scan.sources.push(SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    });
    scan.pressure.get_or_insert_default().request_memory = Some(RequestMemorySnapshot {
        source: coverage.source.clone(),
        observed_at: native.observed_from.clone(),
        observations: observations(rows).into_iter().cloned().collect(),
    });
    scan
}

#[tokio::test]
async fn authenticated_request_memory_reuses_scan_explanations_and_keeps_native_ownership_exact() {
    for (source, rows) in request_memory_evidence_cases() {
        let expected = culprit_checks::analyze(scan(&source, &rows)).request_memory.unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::request_memory(&capture).unwrap();
        assert_eq!(result.report, expected);
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        assert_eq!(result.context.manifest_sha256, fixture.range.manifest_digest());
        for row in &rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(result.context.records.iter().any(|actual| actual.row == expected));
        }
        let keys = observations(&rows).iter().map(|row| row.key().unwrap()).collect::<std::collections::BTreeSet<_>>();
        assert!(result.report.entries.iter().all(|entry| keys.contains(&entry.observation_key)));
        assert!(!serde_json::to_string(&result.report).unwrap().contains("SELECT payload"));
    }
}

#[tokio::test]
async fn missing_rows_and_publication_preserve_partial_readback_separately_from_native_coverage() {
    let (source, rows) = request_memory_evidence_cases().remove(0);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::request_memory(&capture).unwrap();
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert!(result.report.source_reason.is_some());
    assert_eq!(result.report.entries.len(), 1);
    let SourceObservation::Snapshot(native) = &result.context.source.observation else { panic!() };
    assert_eq!(native.sources[0].state, SourceState::Collected);
    assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    fixture.unpublish();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::request_memory(&capture).unwrap();
    assert!(result.report.entries.is_empty());
    assert_eq!(result.report.source_state, Some(SourceState::Partial));
    assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::NotPublished);
}

#[tokio::test]
async fn restricted_and_recovery_only_statements_do_not_invent_an_allocator_or_bypass_payload_proofs() {
    let columns = (0..400).map(|column| format!("field_{column}")).collect::<Vec<_>>().join(", ");
    for statement in [
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
        DefinitionPayload::sql(
            &format!("SELECT {columns} FROM dbo.orders WHERE customer_id=9264"),
            culprit_core::Engine::Mssql,
        ),
    ] {
        let recovery_only = matches!(&statement, DefinitionPayload::Available { text, .. } if text.len() > 1024);
        let (source, rows) = request_memory_evidence(statement, true);
        let fixture = Fixture::new(source, rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::request_memory(&capture).unwrap();
        if recovery_only {
            assert!(result.report.entries.is_empty());
            assert_eq!(result.report.source_state, Some(SourceState::Partial));
            assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
        } else {
            assert!(capture.is_read_complete());
            assert_eq!(result.report.entries.len(), 1);
            assert!(
                result.report.entries[0]
                    .explanation
                    .contains(&culprit_core::t!("view.request_memory.request.statement_unavailable"))
            );
        }
    }
}

#[tokio::test]
async fn wrong_scope_database_dataset_and_observation_bounds_cannot_acquire_request_memory_semantics() {
    for shape in
        ["shared-target", "dataset", "database", "scope", "outside-observation", "wrong-family", "mixed-points"]
    {
        let (mut source, mut rows) = if shape == "mixed-points" {
            request_memory_evidence_cases().remove(0)
        } else {
            request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }, true)
        };
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::ExecutionGrant(grant), ..
        }) = &mut rows[0]
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
            "dataset" => native(&mut source).dataset = SnapshotDataset::Operational,
            "database" => grant.request.as_mut().unwrap().database = Some("another-db".into()),
            "scope" => grant.scope = Some(culprit_core::MeasurementScope::Server),
            "outside-observation" => grant.observed_at = "2020-01-01T12:00:02Z".into(),
            "wrong-family" => {}
            "mixed-points" => grant.observed_at = "2020-01-01T12:00:00.500Z".into(),
            _ => unreachable!(),
        }
        let fixture = Fixture::new(source, rows).await;
        let family =
            if shape == "wrong-family" { EvidenceFamily::ResourceSamples } else { EvidenceFamily::OperationalSamples };
        let capture = fixture.read_families(&[family]).await;
        let error = collected_resources::request_memory(&capture).unwrap_err();
        let problem = if matches!(shape, "shared-target" | "dataset" | "wrong-family") {
            DeliveryProblem::UnsupportedSchema
        } else {
            DeliveryProblem::Integrity
        };
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(problem)), "{shape}");
    }
}

#[tokio::test]
async fn native_local_request_times_remain_independent_from_authenticated_utc_observation_bounds() {
    let (source, mut rows) =
        request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }, true);
    let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
    let OperationalFact::ExecutionGrant(grant) = &mut row.fact else { panic!() };
    grant.requested_without_offset = "2099-01-01T10:00:00.003".into();
    grant.granted_without_offset = Some("2099-01-01T10:00:00.006".into());
    grant.identity.qualifiers.insert("requested_without_offset".into(), grant.requested_without_offset.clone());
    row.key = grant.key().unwrap();
    let fixture = Fixture::new(source, rows.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::request_memory(&capture).unwrap();
    assert_eq!(result.report.entries.len(), 1);
    assert!(result.report.limitations.contains(&culprit_core::t!("view.request_memory.limit.clock")));
    assert_eq!(
        result.context.records[0].row,
        ExportedRecord::new(capture.source(), capture.capture(), rows[0].clone()).unwrap()
    );
}
