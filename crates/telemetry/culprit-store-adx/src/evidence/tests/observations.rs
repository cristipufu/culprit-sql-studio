use super::*;

#[tokio::test]
async fn retained_memory_failures_keep_duplicate_native_rows_signed_values_and_unparsed_documents() {
    let (source, records) = test_support::memory_failure_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    assert_eq!(schema.table(), "CulpritOperationalV1");
    fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
    let result = fixture
        .reader()
        .read_capture(
            &fixture.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget { page_rows: 1, ..budget() },
        )
        .await
        .unwrap();
    assert!(result.is_read_complete(), "{:?}", result.gaps());
    assert_eq!(result.source(), &source);
    assert_eq!(result.metrics().pages, 2);
    assert_eq!(result.records().len(), 2);
    assert_ne!(result.records()[0].row().record_id, result.records()[1].row().record_id);
    let CollectionTarget::Shared(resource) = &result.source().target else { panic!("shared native pool") };
    assert_eq!(resource.kind, SharedResourceKind::Pool);
    for (index, native) in records.iter().enumerate() {
        let expected = ExportedRecord::new(&source, result.capture(), native.clone()).unwrap();
        let actual = &result.records()[index];
        assert_eq!(actual.row(), &expected);
        assert_eq!(actual.row_ordinal() as usize, index + 1);
        let TelemetryRecord::Operational(observation) = &actual.row().record else { panic!() };
        let OperationalFact::MssqlOutOfMemory(row) = &observation.fact else { panic!() };
        assert_eq!(row.position as usize, index);
        assert_eq!(row.event_time_without_offset, "2020-01-01T10:30:00.003");
        assert_eq!(row.available_physical_memory_mb, i32::MIN);
        assert_eq!(row.current_job_object_memory_limit_mb, Some(-1));
        assert_eq!(row.possible_non_sos_leaked_memory_mb, Some(0));
        assert!(row.initial_job_object_memory_limit_mb.is_none());
        assert!(row.top_resource_pools.is_none());
        assert_eq!(row.oom_resource_pools.as_ref().unwrap().raw, "null");
        assert_eq!(row.oom_resource_pools.as_ref().unwrap().qualification, MemoryFailureJsonQualification::Parsed);
        let unparsed = row.possible_leaked_memory_clerks.as_ref().unwrap();
        assert_eq!(unparsed.raw, "{unfinished");
        assert_eq!(unparsed.qualification, MemoryFailureJsonQualification::Unparsed);
        let details = serde_json::to_value(row).unwrap();
        assert!(details.get("query").is_none() && details.get("session").is_none());
        assert!(details.get("database").is_none() && details.get("event_time").is_none());
    }
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    let details_column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut tampered = fixture.rows[0].clone();
    let mut details: Value = serde_json::from_str(tampered[details_column].as_str().unwrap()).unwrap();
    details["possible_leaked_memory_clerks"]["qualification"] = json!("parsed");
    tampered[details_column] = json!(serde_json::to_string(&details).unwrap());
    fixture.replies(vec![vec![tampered]]);
    let error = fixture
        .reader()
        .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn exact_operational_reads_keep_nested_request_sql_grant_unknowns_and_retention_scope() {
    let sql =
        DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql);
    for (source, records) in [
        test_support::request_memory_evidence(sql, true),
        test_support::request_memory_evidence(
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
            true,
        ),
        test_support::request_memory_evidence(
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            false,
        ),
        test_support::retention_evidence(),
        test_support::retention_coverage_only_evidence(vec![CleanupHorizon::Data, CleanupHorizon::Catalog]),
        test_support::retention_coverage_only_evidence(vec![]),
    ] {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let result = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
            .await
            .unwrap();
        assert!(result.is_read_complete(), "{:?}", result.gaps());
        assert_eq!(result.source(), &source);
        assert_eq!(result.records().len(), records.len());
        for native in &records {
            let expected = ExportedRecord::new(&source, result.capture(), native.clone()).unwrap();
            let actual = result.records().iter().find(|record| record.row() == &expected).unwrap();
            let TelemetryRecord::Operational(observation) = &actual.row().record else { panic!() };
            match &observation.fact {
                OperationalFact::ExecutionGrant(grant) => {
                    assert_eq!(grant.requested_kibibytes.as_deref(), Some("9007199254740993"));
                    assert_eq!(grant.used_kibibytes.as_deref(), Some("0"));
                    assert!(grant.resource.is_none());
                    assert!(grant.ideal_kibibytes.is_none());
                    assert_eq!(grant.native_properties["resource_semaphore_id"], "0");
                    if let Some(request) = &grant.request {
                        assert_eq!(request.client.as_ref().unwrap().application.as_deref(), Some("application-worker"));
                        if let DefinitionPayload::Available { text, .. } = &request.statement {
                            assert!(!text.contains("9264"));
                        }
                    } else {
                        assert!(grant.scope.is_none());
                        assert_eq!(grant.request_association, RequestGrantAssociation::MissingRequest);
                        assert!(!source.observation.snapshot().unwrap().sources[0].complete_scope);
                    }
                }
                OperationalFact::Retention(holder) => {
                    assert_eq!(holder.scope, culprit_core::MeasurementScope::Server);
                    assert_eq!(holder.unresolved_session_id.as_deref(), Some("99"));
                    assert!(holder.session.is_none() && holder.age_seconds.is_none());
                    assert_eq!(holder.code_ownership, PayloadUnavailable::NotCollected);
                    assert!(holder.horizons[1].transactions_behind.is_none());
                    let native_source = source
                        .observation
                        .snapshot()
                        .unwrap()
                        .sources
                        .iter()
                        .find(|source| source.source == holder.source)
                        .unwrap();
                    assert!(!native_source.complete_scope);
                    assert_eq!(native_source.records, 1);
                }
                OperationalFact::RetentionCoverage(coverage) => {
                    assert!(!coverage.qualified_horizons.contains(&CleanupHorizon::SharedCatalog));
                    if !coverage.qualified_horizons.is_empty() {
                        assert_eq!(coverage.qualified_horizons, vec![CleanupHorizon::Data, CleanupHorizon::Catalog]);
                    }
                    let snapshot = source.observation.snapshot().unwrap();
                    let metadata =
                        snapshot.sources.iter().find(|source| source.source == RetentionCoverage::SOURCE).unwrap();
                    assert_eq!(metadata.records, 1);
                    assert!(metadata.complete_scope);
                    assert_eq!(metadata.state, culprit_core::SourceState::Collected);
                    assert_eq!(snapshot.records as usize, records.len());
                    assert_eq!(coverage.native_sources, vec!["pg_replication_slots"]);
                    if records.len() == 1 {
                        let native_source =
                            snapshot.sources.iter().find(|source| source.source == "pg_replication_slots").unwrap();
                        assert_eq!(native_source.records, 0);
                        assert_eq!(native_source.state, culprit_core::SourceState::Empty);
                    }
                }
                _ => panic!("unexpected native fact"),
            }
        }
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn a_truncated_nested_request_statement_requires_recovery_without_fetching_native_payloads() {
    let sql = format!("SELECT {}last_column FROM dbo.orders", "column_name,".repeat(200));
    let (source, records) =
        test_support::request_memory_evidence(DefinitionPayload::sql(&sql, culprit_core::Engine::Mssql), true);
    let fixture = Fixture::new(source, records).await;
    fixture.replies(vec![fixture.rows.clone()]);
    let result =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert!(result.records().is_empty());
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}

#[tokio::test]
async fn changed_payload_inventory_pointer_type_or_digest_cannot_hide_behind_a_native_hash() {
    let (source, records) = test_support::request_memory_evidence(
        DefinitionPayload::sql("SELECT payload FROM dbo.orders", culprit_core::Engine::Mssql),
        true,
    );
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "DefinitionPayloads").unwrap();
    for changed in ["pointer", "type", "sha256"] {
        let mut rows = fixture.rows.clone();
        let mut references: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        match changed {
            "pointer" => references[0]["pointer"] = json!("/record/data/fact/data/request/client"),
            "type" => references[0]["preview"] = json!(true),
            _ => references[0]["sha256"] = json!(digest(b"another-payload")),
        }
        rows[0][column] = json!(serde_json::to_string(&references).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::OperationalSamples], budget())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}
