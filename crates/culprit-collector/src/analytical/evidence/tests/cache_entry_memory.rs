use super::*;
use crate::{
    analytical::{catalog::PhysicalCatalog, publication},
    delivery::recovery::CommittedRange,
};
use culprit_core::{DatabaseIdentity, Engine};
use serde_json::{Map, Value, json};

fn project(
    catalog: &LogicalCatalog,
    range: &CommittedRange,
    record: TelemetryRecord,
) -> (ExportedRecord, Map<String, Value>) {
    let batch = &range.manifest().batches[0];
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record).unwrap();
    let input = Envelope::committed(range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let schema = catalog.get(batch.stream).unwrap();
    let values = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    (row, values)
}

#[tokio::test]
async fn whole_entries_keep_all_selected_statements_exact_counts_unknowns_and_native_receipts() {
    let catalog = LogicalCatalog::new().unwrap();
    for (_, source, records) in test_support::cache_entry_memory_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 270_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        for record in records {
            let (row, values) = project(&catalog, &range, record);
            let batch = &range.manifest().batches[0];
            let decode::Decoded::Record(decoded) =
                decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
            else {
                panic!("complete native cache object")
            };
            assert_eq!(*decoded, row);
            assert_eq!(values["Details"]["size_bytes"], "9007199254740993");
            assert_eq!(values["Details"]["reference_count"], "0");
            assert_eq!(values["DefinitionPayloads"].as_array().unwrap().len(), 2);
            assert!(values["Details"]["statements"][0]["statement"].get("text").is_none());
        }
    }
}

#[tokio::test]
async fn every_one_of_eight_statements_is_redacted_and_has_an_exact_indexed_payload_reference() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, mut records) =
        test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::EightStatements);
    let TelemetryRecord::Operational(observation) = &mut records[0] else { panic!() };
    let OperationalFact::MssqlCacheEntryMemory(entry) = &mut observation.fact else { panic!() };
    for (index, statement) in entry.statements.iter_mut().enumerate() {
        let text = format!("SELECT value_{index} FROM dbo.cache_input WHERE id=9264");
        statement.statement = DefinitionPayload::Available {
            sha256: digest(text.as_bytes()),
            text,
            redaction_version: REDACTION_VERSION,
        };
    }
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 270_001);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let (row, values) = project(&catalog, &range, records[0].clone());
    assert!(!serde_json::to_string(&row).unwrap().contains("9264"));
    let definitions = values["DefinitionPayloads"].as_array().unwrap();
    assert_eq!(definitions.len(), MAX_CACHE_ENTRY_STATEMENTS);
    for (index, definition) in definitions.iter().enumerate() {
        let pointer = format!("/record/data/fact/data/statements/{index}/statement");
        assert_eq!(definition["pointer"], pointer);
        assert_eq!(&values["Details"]["statements"][index]["statement"], definition);
        assert!(!definition["preview"].as_str().unwrap().contains("9264"));
    }
    let batch = &range.manifest().batches[0];
    let decode::Decoded::Record(decoded) = decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
    else {
        panic!("all eight complete native statements")
    };
    assert_eq!(*decoded, row);
}

#[tokio::test]
async fn eight_long_sql_payloads_require_recovery_and_copied_or_missing_indexed_references_fail_closed() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) =
        test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::EightStatements);
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 270_002);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let (_, values) = project(&catalog, &range, records[0].clone());
    let batch = &range.manifest().batches[0];
    assert_eq!(values["DefinitionPayloads"].as_array().unwrap().len(), MAX_CACHE_ENTRY_STATEMENTS);
    for definition in values["DefinitionPayloads"].as_array().unwrap() {
        assert_eq!(definition["preview_truncated"], true);
    }
    assert!(matches!(
        decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
    ));
    for mutation in 0..3 {
        let mut changed = values.clone();
        match mutation {
            0 => {
                changed.get_mut("DefinitionPayloads").unwrap()[0]["pointer"] =
                    json!("/record/data/fact/data/statements/7/statement")
            }
            1 => {
                changed.get_mut("DefinitionPayloads").unwrap().as_array_mut().unwrap().pop();
            }
            _ => {
                changed.get_mut("Details").unwrap()["statements"][0]["statement"]["sha256"] =
                    json!(digest(b"foreign statement"))
            }
        }
        assert!(
            !matches!(decode::record(&range, batch.stream, &changed, &batch.rows[0]), Ok(decode::Decoded::Record(_))),
            "mutation {mutation} cannot reconstruct a native row without the exact inventory"
        );
    }
}

#[tokio::test]
async fn valid_but_different_focus_and_generic_snapshot_cannot_authorize_native_entries_or_empty_ranges() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::Complete);
    let capture = test_support::fixture().0.capture;
    let SourceObservation::CacheEntryMemory { focus, .. } = &source.observation else { panic!() };
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        budget: CollectionBudget::default(),
        selection: CollectionSelection::CacheEntryMemory { focus: focus.clone() },
    };
    assert!(source.matches_request(&request));
    for mutation in 0..3 {
        let mut changed = source.clone();
        let SourceObservation::CacheEntryMemory { focus, .. } = &mut changed.observation else { panic!() };
        match mutation {
            0 => focus.statements[0].identity.plan_generation = "2".into(),
            1 => {
                focus.statements.remove(0);
            }
            _ => focus.statements[0].database_id = 8,
        }
        if mutation == 2 {
            for statement in &mut focus.statements {
                statement.database_id = 8;
            }
        }
        assert!(focus.is_valid());
        assert!(!changed.matches_request(&request));
        assert!(ExportedRecord::new(&changed, &capture, records[0].clone()).is_err());
    }
    let bare_request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CacheEntryMemory },
        ..request
    };
    assert!(bare_request.validate().is_err());
    assert!(!source.matches_request(&bare_request));
    let (empty, no_records) = test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::Empty);
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), empty, no_records, 270_003);
    let mut manifest = recovery.manifest.clone();
    let SourceObservation::CacheEntryMemory { snapshot, .. } = &manifest.source.observation else { panic!() };
    manifest.source.observation = SourceObservation::Snapshot(snapshot.clone());
    assert!(!manifest.source.matches_request(&bare_request));
    assert!(manifest.validate().is_err());
}

#[tokio::test]
async fn published_and_row_metadata_keep_only_the_focus_key_without_private_selection_context() {
    let catalog = PhysicalCatalog::<test_support::JsonProjection>::new().unwrap();
    let (mut source, records) = test_support::cache_entry_memory_evidence(test_support::CacheEntryMemoryCase::Complete);
    source.target = DatabaseIdentity::server(
        Engine::Mssql,
        "private-cache-database",
        "private-cache-endpoint.invalid",
        1433,
        Some("private-native-database-guid".into()),
    )
    .into();
    let SourcePartition::InstanceLocal { member, epoch } = &mut source.partition else { panic!() };
    let member = member.as_mut().unwrap();
    member.namespace = NativeIdentity::new("private-instance-namespace");
    member.native = NativeIdentity::new("private-compute-member");
    *epoch = Some("private-process-generation".into());
    let SourceObservation::CacheEntryMemory { focus, .. } = &mut source.observation else { panic!() };
    focus.context.target = source.target.clone();
    focus.context.partition = source.partition.clone();
    let focus_key = focus.key().unwrap();
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 270_004);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let published = publication::input(&catalog, &range).unwrap();
    let public = &published["source"]["observation"];
    assert_eq!(public["kind"], "cache-entry-memory-context");
    assert_eq!(public["data"]["focus_key"], focus_key);
    assert_eq!(public["data"].as_object().unwrap().len(), 2);
    assert!(serde_json::from_value::<SourceObservation>(public.clone()).is_err());
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[0].clone()).unwrap();
    let input = Envelope::committed(&range, &range.manifest().batches[0].batch_id).unwrap().input(1, &row).unwrap();
    assert_eq!(input["_culprit"]["focus_key"], focus_key);
    assert_eq!(input["_culprit"]["range_kind"], "focused");
    for metadata in [&published, &input["_culprit"]] {
        let json = serde_json::to_string(metadata).unwrap();
        for private in [
            "private-cache",
            "private-native-database-guid",
            "private-instance-namespace",
            "private-compute-member",
            "private-process-generation",
            "0x0102",
            "0x0304",
            "source_text_bytes",
        ] {
            assert!(!json.contains(private), "private selection metadata: {private}");
        }
    }
}
