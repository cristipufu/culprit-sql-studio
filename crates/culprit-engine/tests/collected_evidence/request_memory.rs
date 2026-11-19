use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{
        focused_request_memory_evidence_with_peers, request_memory_evidence, request_memory_evidence_cases,
    },
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

#[tokio::test]
async fn authenticated_workload_group_tempdb_replays_shared_history_without_a_query_failure_or_effective_cap() {
    let (source, rows) = request_memory_evidence_cases()
        .into_iter()
        .find(|(_, rows)| observations(rows).iter().any(|row| row.workload_group_tempdb.is_some()))
        .expect("native-shaped group tempdb corpus");
    let expected = culprit_checks::analyze(scan(&source, &rows)).request_memory.unwrap();
    let fixture = Fixture::new(source.clone(), rows.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let result = collected_resources::request_memory(&capture).unwrap();
    assert_eq!(result.context.source, source);
    assert_eq!(result.report, expected);
    assert_eq!(result.report.entries.len(), 2);
    for entry in &result.report.entries {
        assert!(entry.explanation.contains(&culprit_core::t!("view.request_memory.tempdb.current", value = "0")));
        assert!(
            entry
                .explanation
                .contains(&culprit_core::t!("view.request_memory.tempdb.peak", value = "9007199254740993"))
        );
        assert!(entry.explanation.contains(&culprit_core::t!("view.request_memory.tempdb.violations", count = "3")));
        assert!(!entry.explanation.contains(&culprit_core::t!("view.request_memory.tempdb.violations", count = "6")));
        assert!(entry.qualified_request.is_some());
    }
    assert!(result.report.limitations.contains(&culprit_core::t!("view.request_memory.tempdb.limit")));
    for record in &rows {
        let exported = ExportedRecord::new(&source, capture.capture(), record.clone()).unwrap();
        assert!(result.context.records.iter().any(|row| row.row == exported));
    }
    let content = serde_json::to_string(&result.report).unwrap();
    for private in ["SELECT payload", "9264", "fix_sql", "causal_share", "severity"] {
        assert!(!content.contains(private));
    }
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
    let native = source.observation.snapshot().expect("native request-memory fixture");
    let coverage = &native.sources[0];
    scan.sources.retain(|source| source.source != coverage.source);
    scan.sources.push(SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    });
    scan.pressure.get_or_insert_default().request_memory = Some(RequestMemorySnapshot {
        context: NativeReadContext::from_snapshot(source).unwrap(),
        source: coverage.source.clone(),
        observed_at: native.observed_from.clone(),
        observations: observations(rows).into_iter().cloned().collect(),
    });
    scan
}

#[tokio::test]
async fn authenticated_request_memory_reuses_scan_explanations_and_keeps_native_ownership_exact() {
    for (source, rows) in
        request_memory_evidence_cases().into_iter().chain([focused_request_memory_evidence_with_peers(3)])
    {
        let expected = culprit_checks::analyze(scan(&source, &rows)).request_memory.unwrap();
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let recovery_before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let result = collected_resources::request_memory(&capture).unwrap();
        assert_eq!(result.report, expected);
        assert_eq!(result.context.source, source);
        assert_eq!(result.context.records.len(), rows.len());
        assert_eq!(result.context.recovery_manifest_sha256, fixture.range.manifest_digest());
        for row in &rows {
            let expected = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(result.context.records.iter().any(|actual| actual.row == expected));
        }
        let keys = observations(&rows).iter().map(|row| row.key().unwrap()).collect::<std::collections::BTreeSet<_>>();
        assert!(result.report.entries.iter().all(|entry| keys.contains(&entry.observation_key)));
        assert!(!serde_json::to_string(&result.report).unwrap().contains("SELECT payload"));
        assert_eq!(fixture.recovery_object_reads(), recovery_before);
        if let SourceObservation::FocusedRequestMemory { result: focused, .. } = &source.observation {
            let raw = observations(&rows);
            assert_eq!(focused.peers.len(), 3);
            assert!(focused.matches_observations(raw.iter().copied()));
            assert_eq!(result.report.admission_resources.len(), 1);
            let resource = &result.report.admission_resources[0];
            assert_eq!(Some(&resource.resource), focused.resource.as_ref());
            assert_eq!(resource.participants.len(), 4);
            for grant in raw {
                assert!(
                    resource.participants.iter().any(|participant| participant.observation_identity == grant.identity)
                );
                if focused.is_peer(grant) {
                    assert_eq!(grant.statement_relationship, GrantStatementRelationship::Unresolved);
                    assert!(matches!(
                        grant.request.as_ref().unwrap().statement,
                        DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
                    ));
                }
            }
        }
    }
}

#[tokio::test]
async fn authenticated_admission_condition_keeps_original_shared_counters_and_acquisition_context() {
    let (source, rows) = request_memory_evidence_cases()
        .into_iter()
        .find(|(_, rows)| observations(rows).iter().any(|row| row.resource_observation.is_some()))
        .expect("native shared-resource corpus");
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    assert!(context.has_qualified_local_owner());
    assert!(matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
        if member.kind == SharedResourceKind::Instance));
    let database = source.target.database().unwrap();
    assert!(observations(&rows).iter().all(|row| row.is_admission_participant(&database.database)));
    let original_waiter =
        observations(&rows).into_iter().find(|row| row.state == ExecutionGrantState::Waiting).unwrap();
    let fixture = Fixture::new(source.clone(), rows.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(capture.is_read_complete());
    let result = collected_resources::request_memory(&capture).unwrap();
    let entry =
        result.report.entries.iter().find(|entry| entry.observation_identity == original_waiter.identity).unwrap();
    assert!(entry.explanation.contains(&culprit_core::t!(
        "view.request_memory.resource.below_required",
        available = "-64",
        required = "64",
        requested = "1024"
    )));
    assert!(entry.explanation.contains(&culprit_core::t!("view.request_memory.resource.limit")));
    assert!(entry.mechanism.is_none());
    let original = result
        .context
        .records
        .iter()
        .find_map(|row| match &row.row.record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) if grant.identity == original_waiter.identity => Some(grant.as_ref()),
            _ => None,
        })
        .unwrap();
    assert_eq!(original, original_waiter);
    assert_eq!(result.report, culprit_checks::analyze(scan(&source, &rows)).request_memory.unwrap());
    assert!(result.report.limitations.contains(&culprit_core::t!("view.request_memory.limit.population")));
}

#[tokio::test]
async fn a_missing_focused_subject_or_peer_remains_partial_without_completing_the_sealed_roster() {
    for missing_subject in [true, false] {
        let (source, mut rows) = focused_request_memory_evidence_with_peers(3);
        if !missing_subject {
            rows.swap(0, 1);
        }
        let fixture = Fixture::new(source.clone(), rows).await;
        let recovery_before = fixture.recovery_object_reads();
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(!capture.is_read_complete());
        let result = collected_resources::request_memory(&capture).unwrap();
        assert_eq!(result.context.source, source);
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert_eq!(
            result.report.source_reason.as_deref(),
            Some(culprit_core::t!("engine.collected_resources.incomplete_read").as_str())
        );
        assert_eq!(result.context.records.len(), 3);
        assert_eq!(result.report.entries.len(), 3);
        assert_eq!(result.context.read_gaps.len(), 1);
        assert_eq!(result.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
        let SourceObservation::FocusedRequestMemory { result: focused, snapshot } = &result.context.source.observation
        else {
            panic!()
        };
        assert_eq!(snapshot.records, 4);
        assert_eq!(snapshot.sources[0].state, SourceState::Collected);
        assert_eq!(focused.peers.len(), 3);
        let native = result
            .context
            .records
            .iter()
            .map(|row| match &row.row.record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(grant),
                    ..
                }) => grant.as_ref(),
                _ => panic!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(native.iter().any(|grant| focused.is_subject(grant)), !missing_subject);
        assert!(native.iter().all(|grant| focused.accepts_observation(grant)));
        assert!(!focused.matches_observations(native));
        assert_eq!(result.report.admission_resources.len(), 1);
        assert_eq!(
            result.report.admission_resources[0].participants.len(),
            3,
            "the view references only the readable subset"
        );
        assert!(result.report.limitations.contains(&culprit_core::t!("view.request_memory.limit.population")));
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery_object_reads(), recovery_before);
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
        for native_statement_observed in [false, true] {
            let recovery_only = matches!(&statement, DefinitionPayload::Available { text, .. } if text.len() > 1024);
            let (source, mut rows) = request_memory_evidence(statement.clone(), true);
            assert!(NativeReadContext::from_snapshot(&source).unwrap().has_qualified_local_owner());
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) = &mut rows[0]
            else {
                panic!("native grant fixture")
            };
            let request = grant.request.as_mut().unwrap();
            request.statement_identity =
                native_statement_observed.then(|| NativeIdentity::new("native-current-statement"));
            let original_request = request.key.clone();
            assert!(grant.is_valid());
            let fixture = Fixture::new(source, rows).await;
            let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
            let result = collected_resources::request_memory(&capture).unwrap();
            assert_eq!(result.context.source.observation.snapshot().unwrap().records, 1);
            if recovery_only {
                assert!(result.report.entries.is_empty());
                assert_eq!(result.report.source_state, Some(SourceState::Partial));
                assert!(result.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
            } else {
                assert!(capture.is_read_complete());
                assert_eq!(result.report.entries.len(), 1);
                let entry = &result.report.entries[0];
                assert_eq!(entry.qualified_request.as_ref(), native_statement_observed.then_some(&original_request));
                let message = if native_statement_observed {
                    "view.request_memory.request.statement_unavailable"
                } else {
                    "view.request_memory.request.unqualified"
                };
                assert!(entry.explanation.contains(&culprit_core::i18n::t(message, &[])));
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::ExecutionGrant(retained),
                    ..
                }) = &capture.records()[0].row().record
                else {
                    panic!("retained grant")
                };
                assert!(matches!(
                    retained.request.as_ref().unwrap().statement,
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
                ));
                assert_eq!(retained.request.as_ref().unwrap().statement_identity.is_some(), native_statement_observed);
            }
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
