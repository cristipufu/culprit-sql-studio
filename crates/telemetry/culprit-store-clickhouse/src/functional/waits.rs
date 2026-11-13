use super::*;
use culprit_collector::analytical::{evidence::*, indexes::ProjectionIndexes};

#[tokio::test]
#[ignore = "requires the owned ClickHouse container from dev/smoke-clickhouse.py"]
async fn authenticated_wait_meanings_preserve_empty_unknown_and_missing_native_evidence() {
    let server = Server::new();
    server.deploy();
    let catalog = SchemaCatalog::new().unwrap();
    let families = [EvidenceFamily::QueryWaits];
    let budget = EvidenceReadBudget::default();
    for (index, (source, records)) in test_support::query_wait_evidence_cases().into_iter().enumerate() {
        let scenario = Scenario::new(
            &server,
            Recovery::capture(
                server.client.destination(),
                catalog.digest(),
                source.clone(),
                records.clone(),
                130_000 + index as u64,
            ),
        )
        .await;
        scenario.complete().await;
        *scenario.recovery.fault.lock().unwrap() = Fault::Stall;
        let reader =
            crate::evidence::new(server.client.clone(), Arc::new(ProjectionIndexes::new(scenario.indexes.clone())))
                .unwrap();
        let result = reader.read_capture(&scenario.range, &families, budget).await.unwrap();
        assert!(result.is_read_complete(), "{:?}", result.gaps());
        assert_eq!(result.source(), &source);
        let expected =
            records.into_iter().filter(|record| record.stream() == TelemetryStream::QueryWaits).collect::<Vec<_>>();
        assert_eq!(result.records().len(), expected.len());
        for native in expected {
            let exported = ExportedRecord::new(&source, result.capture(), native).unwrap();
            assert!(result.records().iter().any(|actual| actual.row() == &exported));
        }
        if result.records().is_empty() {
            assert_eq!(result.metrics().queries, 2);
            assert_eq!(source.query_history().unwrap().waits.as_ref().unwrap().capture, WaitCaptureState::Enabled);
        }
        let unverified = source
            .query_history()
            .unwrap()
            .waits
            .as_ref()
            .is_some_and(|waits| waits.coverage == WaitCaptureCoverage::Unverified);
        if index != 0 && !unverified {
            continue;
        }
        let range = &scenario.range.manifest().range_id;
        if index == 0 {
            let waits = crate::schema::quote(catalog.get(TelemetryStream::QueryWaits).unwrap().table());
            server.execute(&format!("ALTER TABLE {waits} DELETE WHERE RangeId='{range}' SETTINGS mutations_sync=2"));
            let missing = reader.read_capture(&scenario.range, &families, budget).await.unwrap();
            assert!(!missing.is_read_complete());
            assert_eq!(missing.source(), &source);
            assert_eq!(missing.gaps().len(), 1);
            assert_eq!(missing.gaps()[0].stream, Some(TelemetryStream::QueryWaits));
            assert_eq!(missing.gaps()[0].reason, EvidenceGapReason::MissingRows);
        }

        let publication = crate::schema::quote(catalog.publication().table());
        let rows = server
            .values(
                format!("SELECT SourceDescription FROM {publication} WHERE RangeId={{range:String}}"),
                [("range".into(), range.clone())].into(),
                &[("SourceDescription", "String")],
            )
            .await;
        assert_eq!(rows.len(), 1);
        let mut description: Value = serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap();
        if unverified {
            *description.pointer_mut("/observation/data/waits/coverage").unwrap() = json!("qualified");
        } else {
            *description.pointer_mut("/observation/data/waits/categories/native-page-io/disposition").unwrap() =
                json!({"kind":"analyze","class":"lock"});
        }
        let changed = serde_json::to_string(&description).unwrap().replace('\'', "''");
        server.execute(&format!("INSERT INTO {publication} SELECT * REPLACE('{changed}' AS SourceDescription) FROM {publication} WHERE RangeId='{range}' LIMIT 1"));
        let error = reader.read_capture(&scenario.range, &families, budget).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}
