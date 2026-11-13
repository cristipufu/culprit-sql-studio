use super::*;
use culprit_collector::analytical::{evidence::*, indexes::ProjectionIndexes};

#[tokio::test]
#[ignore = "requires the owned ClickHouse container from dev/smoke-clickhouse.py"]
async fn typed_evidence_reads_keep_nanoseconds_unknowns_scope_and_native_counter_epochs() {
    let server = Server::new();
    server.deploy();
    let catalog = SchemaCatalog::new().unwrap();
    let budget = EvidenceReadBudget { page_bytes: 1024 * 1024, max_bytes: 1024 * 1024, ..Default::default() };
    let (source, mut record) = test_support::pool_history();
    let TelemetryRecord::ResourceSample(sample) = &mut record else { panic!() };
    sample.at = "2020-01-01T06:00:15.000000001Z".into();
    let scenario = Scenario::new(
        &server,
        Recovery::capture(server.client.destination(), catalog.digest(), source, vec![record], 80_000),
    )
    .await;
    let reader =
        crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
            .unwrap();
    let unpublished = reader.read_capture(&scenario.range, &[EvidenceFamily::ResourceSamples], budget).await.unwrap();
    assert!(!unpublished.is_read_complete());
    assert_eq!(unpublished.gaps()[0].reason, EvidenceGapReason::NotPublished);
    scenario.complete().await;
    *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
    let result = reader.read_capture(&scenario.range, &[EvidenceFamily::ResourceSamples], budget).await.unwrap();
    assert!(result.is_read_complete());
    let TelemetryRecord::ResourceSample(sample) = &result.records()[0].row().record else { panic!() };
    assert_eq!(sample.at, "2020-01-01T06:00:15.000000001Z");
    assert!(sample.replica_id.is_none());
    assert_eq!(sample.metrics[0].value, Some(0.0));
    assert!(sample.metrics[1].value.is_none());
    assert_eq!(sample.metrics[3].scope, culprit_core::MeasurementScope::Server);
    assert!(!sample.metrics[3].describes_subject(&result.source().target));

    let scenario =
        Scenario::new(&server, test_support::shared_operational_capture(server.client.destination(), catalog.digest()))
            .await;
    scenario.complete().await;
    *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
    let reader =
        crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
            .unwrap();
    let result = reader
        .read_capture(
            &scenario.range,
            &[EvidenceFamily::OperationalSamples],
            EvidenceReadBudget { page_rows: 1, ..budget },
        )
        .await
        .unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.records().len(), 4);
    assert_eq!(result.metrics().pages, 4);
    let mut kinds = std::collections::BTreeSet::new();
    for record in result.records() {
        let TelemetryRecord::Operational(observation) = &record.row().record else { panic!() };
        match &observation.fact {
            OperationalFact::CounterSet(set) => {
                kinds.insert("counter-set");
                assert_eq!(set.scope, culprit_core::MeasurementScope::Server);
                assert_eq!(set.counters[0].value, MeasurementValue::Integer("0".into()));
                assert_eq!(set.counters[1].value, MeasurementValue::Integer("9007199254740993".into()));
                assert_eq!(
                    set.counters[1].reset,
                    culprit_core::ResetMarker::At("2020-01-01T01:00:00.000000001Z".into())
                );
                assert_eq!(set.properties["native_nulls"], "committed_kb");
                assert_eq!(set.properties["unavailable_columns"], "target_kb");
            }
            OperationalFact::Counter(counter) => {
                kinds.insert("counter");
                assert_eq!(counter.value, MeasurementValue::Integer("0".into()));
                assert_eq!(counter.unit, CounterUnit::Count);
                assert_eq!(counter.scope, culprit_core::MeasurementScope::Server);
            }
            OperationalFact::Space(space) => {
                kinds.insert("space");
                assert_eq!(space.scope, culprit_core::FileScope::Server);
                assert!(space.used_bytes.is_none());
                assert_eq!(space.total_bytes, Some(1024.0));
                assert_eq!(space.parts[0].bytes, Some(0.0));
                assert!(space.parts[1].bytes.is_none());
            }
            _ => panic!("unexpected fixture fact"),
        }
    }
    assert_eq!(kinds, std::collections::BTreeSet::from(["counter-set", "counter", "space"]));

    let (source, mut records) = test_support::query_evidence();
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[2] else { panic!() };
    runtime.first_execution_at = Some("2020-01-01T06:00:00.123456001Z".into());
    let scenario = Scenario::new(
        &server,
        Recovery::capture(server.client.destination(), catalog.digest(), source.clone(), records.clone(), 90_000),
    )
    .await;
    scenario.complete().await;
    *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
    let reader =
        crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
            .unwrap();
    let families = [
        EvidenceFamily::QueryDefinitions,
        EvidenceFamily::PlanDefinitions,
        EvidenceFamily::QueryRuntime,
        EvidenceFamily::QueryWaits,
    ];
    let result =
        reader.read_capture(&scenario.range, &families, EvidenceReadBudget { page_rows: 1, ..budget }).await.unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.source(), &source);
    assert_eq!(result.records().len(), records.len());
    for record in records {
        let expected = ExportedRecord::new(&source, result.capture(), record).unwrap();
        assert_eq!(
            result.records().iter().find(|record| record.row().record_id == expected.record_id).unwrap().row(),
            &expected
        );
    }
    let table = crate::schema::quote(catalog.get(TelemetryStream::PlanDefinitions).unwrap().table());
    server.execute(&format!(
        "INSERT INTO {table} SELECT * REPLACE('{}' AS QueryDefinitionId) FROM {table} WHERE RangeId='{}' LIMIT 1",
        digest(b"another-native-query-parent"),
        scenario.range.manifest().range_id,
    ));
    let error = reader.read_capture(&scenario.range, &families, budget).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));

    let sql =
        DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql);
    for (index, (source, records)) in [
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
        test_support::memory_failure_evidence(),
    ]
    .into_iter()
    .enumerate()
    {
        let scenario = Scenario::new(
            &server,
            Recovery::capture(
                server.client.destination(),
                catalog.digest(),
                source.clone(),
                records.clone(),
                110_000 + index as u64,
            ),
        )
        .await;
        scenario.complete().await;
        *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
        let reader =
            crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
                .unwrap();
        let result = reader.read_capture(&scenario.range, &[EvidenceFamily::OperationalSamples], budget).await.unwrap();
        assert!(result.is_read_complete(), "{:?}", result.gaps());
        assert_eq!(result.source(), &source);
        assert_eq!(result.records().len(), records.len());
        for native in &records {
            let expected = ExportedRecord::new(&source, result.capture(), native.clone()).unwrap();
            assert!(result.records().iter().any(|record| record.row() == &expected));
        }
    }
    let sql = format!("SELECT {}last_column FROM dbo.orders", "column_name,".repeat(200));
    let (source, records) =
        test_support::request_memory_evidence(DefinitionPayload::sql(&sql, culprit_core::Engine::Mssql), true);
    let scenario = Scenario::new(
        &server,
        Recovery::capture(server.client.destination(), catalog.digest(), source, records, 110_100),
    )
    .await;
    scenario.complete().await;
    *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
    let reader =
        crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
            .unwrap();
    let result = reader.read_capture(&scenario.range, &[EvidenceFamily::OperationalSamples], budget).await.unwrap();
    assert!(!result.is_read_complete());
    assert!(result.records().is_empty());
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
}
