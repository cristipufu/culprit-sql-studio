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
}
