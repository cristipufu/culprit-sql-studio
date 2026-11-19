use super::*;
use crate::delivery::recovery::RecoveryRecords;

#[tokio::test]
async fn object_definitions_reconstruct_exact_native_versions_contexts_and_payload_availability() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::object_definition_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 150_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch =
            range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::ObjectDefinitions).unwrap();
        let schema = catalog.get(batch.stream).unwrap();
        for (index, record) in records.iter().enumerate() {
            let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record.clone()).unwrap();
            let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &row).unwrap();
            let values = schema
                .fields()
                .iter()
                .zip(schema.project(&input).unwrap())
                .map(|(field, value)| (field.name.into(), value))
                .collect();
            let decode::Decoded::Record(decoded) =
                decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
            else {
                panic!("inline typed definitions preserve exact native metadata and explicit absent payloads")
            };
            assert_eq!(*decoded, row);
            for column in ["DefinitionVersion", "FactKey"] {
                let mut forged = values.clone();
                forged.insert(column.into(), serde_json::json!(digest(b"foreign-definition")));
                assert!(
                    !matches!(
                        decode::record(&range, batch.stream, &forged, &batch.rows[index]),
                        Ok(decode::Decoded::Record(_))
                    ),
                    "{column} must match native definition and receipt"
                );
            }
            let mut unavailable = values.clone();
            unavailable.insert("DetailState".into(), serde_json::json!("recovery"));
            assert!(matches!(
                decode::record(&range, batch.stream, &unavailable, &batch.rows[index]).unwrap(),
                decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
            ));
        }
    }
}

#[tokio::test]
async fn current_table_type_context_survives_shared_projection_and_recovery_without_rebinding_original_kind() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::object_definition_bound_join_evidence() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 150_001);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let manifest = range.manifest();
        let batch = manifest.batches.iter().find(|batch| batch.stream == TelemetryStream::ObjectDefinitions).unwrap();
        let mut recovered = RecoveryRecords::open(&recovery, batch).await.unwrap();
        let row = recovered.next().await.unwrap().unwrap();
        assert!(recovered.next().await.unwrap().is_none(), "the whole native object must pass its EOF digest");
        assert_eq!(row, ExportedRecord::new(&manifest.source, &manifest.capture, records[0].clone()).unwrap());
        batch.validate_row(&manifest.source, &manifest.capture, 1, &row).unwrap();

        let TelemetryRecord::ObjectDefinition(original) = &row.record else { panic!("native definition") };
        let (current, context, proof) = match &original.fact {
            ObjectDefinitionFact::MssqlCachedDefinition(value) => {
                let proof = value.relational_input_proof.as_ref().unwrap();
                let mut without_current = value.clone();
                without_current.current_type_schema = None;
                assert!(proof.matches_cached(&manifest.source, &without_current));
                (value.current_type_schema.as_ref().unwrap(), &value.context, proof)
            }
            ObjectDefinitionFact::MssqlRetainedPlan(value) => {
                let proof = value.relational_input_proof.as_ref().unwrap();
                let mut without_current = value.clone();
                without_current.current_type_schema = None;
                assert!(proof.matches_retained(&manifest.source, &without_current));
                (value.current_type_schema.as_ref().unwrap(), &value.context, proof)
            }
            _ => panic!("controlled cached or retained definition"),
        };
        assert!(original.fact.native_plan_proofs_match(&manifest.source));
        assert_eq!(current.observed_at, "2020-01-01T12:00:00.7500000Z");
        assert_ne!(current.observed_at, context.identity.created_without_offset);
        let data_type = &current.types[0];
        assert_eq!(data_type.columns.iter().map(|column| column.max_length).collect::<Vec<_>>(), [128, -1, 9]);
        assert_eq!(data_type.columns[0].collation.as_deref(), Some("Latin1_General_100_BIN2"));
        assert_eq!((data_type.columns[2].precision, data_type.columns[2].scale), (18, 3));
        assert!(data_type.columns[1].nullable);
        assert!(!data_type.memory_optimized);
        let index = &data_type.indexes[0];
        assert_eq!((index.native_type.as_str(), index.unique), ("NONCLUSTERED", true));
        assert_eq!((index.columns[0].key_ordinal, index.columns[0].descending), (1, true));
        assert_eq!((index.columns[1].key_ordinal, index.columns[1].included), (0, true));

        let envelope = Envelope::committed(&range, &batch.batch_id).unwrap();
        let schema = catalog.get(batch.stream).unwrap();
        let input = envelope.input(1, &row).unwrap();
        let values: serde_json::Map<_, _> = schema
            .fields()
            .iter()
            .zip(schema.project(&input).unwrap())
            .map(|(field, value)| (field.name.into(), value))
            .collect();
        assert_eq!(values["Details"]["current_type_schema"], serde_json::to_value(current).unwrap());
        assert_eq!(values["Details"]["relational_input_proof"], serde_json::to_value(proof).unwrap());
        assert_eq!(values["DefinitionVersion"], original.version);

        let mut changed_fact = original.fact.clone();
        let changed_current = match &mut changed_fact {
            ObjectDefinitionFact::MssqlCachedDefinition(value) => value.current_type_schema.as_mut().unwrap(),
            ObjectDefinitionFact::MssqlRetainedPlan(value) => value.current_type_schema.as_mut().unwrap(),
            _ => unreachable!(),
        };
        changed_current.types[0].columns[0].max_length = 64;
        let changed = ObjectDefinitionRecord::new(original.source.clone(), changed_fact).unwrap();
        assert!(
            changed.fact.native_plan_proofs_match(&manifest.source),
            "current metadata cannot rebind old kind proof"
        );
        assert_ne!(changed.version, original.version, "the immutable definition includes the independent metadata");
        let changed_row =
            ExportedRecord::new(&manifest.source, &manifest.capture, TelemetryRecord::ObjectDefinition(changed))
                .unwrap();
        assert!(batch.validate_row(&manifest.source, &manifest.capture, 1, &changed_row).is_err());
        assert!(
            envelope.input(1, &changed_row).is_err(),
            "fresh native hashes cannot replace the original row receipt"
        );
    }
}
