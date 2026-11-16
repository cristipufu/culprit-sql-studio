use super::*;
use std::collections::BTreeSet;

fn non_index_cases() -> impl Iterator<Item = (CollectionDescription, Vec<TelemetryRecord>)> {
    test_support::maintenance_progress_evidence_cases().into_iter().filter(|(_, rows)| {
        rows.iter().any(|row| matches!(row,
            TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MaintenanceProgress(point), .. })
                if matches!(point.work,
                    MaintenanceProgressWork::PostgreSqlVacuum { .. }
                    | MaintenanceProgressWork::PostgreSqlAnalyze { .. }
                    | MaintenanceProgressWork::PostgreSqlCluster { .. }
                )
        ))
    })
}

#[tokio::test]
async fn non_index_command_work_counter_and_original_request_tampering_fail_the_native_receipt() {
    let mut seen = BTreeSet::new();
    for (source, records) in non_index_cases() {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MaintenanceProgress(point),
            ..
        }) = &records[0]
        else {
            panic!()
        };
        if !seen.insert(point.command.as_native()) {
            continue;
        }
        let counter = match point.work {
            MaintenanceProgressWork::PostgreSqlVacuum { .. } => "/work/progress/heap_blks_scanned",
            MaintenanceProgressWork::PostgreSqlAnalyze { .. } => "/work/progress/sample_blks_scanned",
            MaintenanceProgressWork::PostgreSqlCluster { .. } => "/work/progress/heap_tuples_written",
            _ => unreachable!(),
        };
        let replacement = if point.command == MaintenanceCommand::Analyze { "vacuum" } else { "analyze" };
        let fixture = Fixture::new(source, records).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
        for (pointer, value) in [
            ("/command", json!(replacement)),
            ("/work/kind", json!("ysql-index-build")),
            ("/actor/request/generation", json!("2020-01-01T11:59:58Z")),
            ("/statement/id", json!("another-native-statement")),
            ("/database_oid", json!("16385")),
            (counter, json!("1")),
        ] {
            let mut rows = fixture.rows.clone();
            let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
            *details.pointer_mut(pointer).unwrap() = value;
            rows[0][column] = json!(serde_json::to_string(&details).unwrap());
            fixture.replies(vec![rows]);
            let error = fixture
                .reader()
                .read_capture(
                    &fixture.range,
                    FAMILY,
                    EvidenceReadBudget {
                        recovery_payloads: Some(RecoveryPayloadBudget::default()),
                        ..Default::default()
                    },
                    Some(fixture.recovery.as_ref()),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.detail,
                Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
                "{pointer}"
            );
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        }
    }
    assert_eq!(seen.len(), 4);
}

#[tokio::test]
async fn non_index_public_header_cannot_replace_original_count_focus_role_or_membership() {
    for (pointer, value) in [
        ("/observation/data/snapshot/records", json!(0)),
        ("/observation/data/focus_key", json!(digest(b"another-original-maintenance-request"))),
        ("/observation/data/database_writable", json!(false)),
        ("/observation/data/membership", json!("not-observed")),
        ("/observation/data/binding", json!("unverified-original-scope")),
    ] {
        let (source, records) = non_index_cases().next().unwrap();
        let mut fixture = Fixture::new(source, records).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let column = fixture
            .catalog
            .publication()
            .columns()
            .iter()
            .position(|column| column.name == "SourceDescription")
            .unwrap();
        let mut description: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        *description.pointer_mut(pointer).unwrap() = value;
        fixture.publication[column] = json!(serde_json::to_string(&description).unwrap());
        fixture.replies(vec![fixture.rows.clone()]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    }
}
