//! Private conformance export from the existing typed corpus through original recovery and shared projection.
//! No provider opens, SQL, source qualification, replacement model definitions or runtime registry.
use culprit_collector::{
    analytical::{catalog::LogicalCatalog, envelope::Envelope, test_support as corpus},
    delivery::recovery::RecoveryRecords,
};
use culprit_core::{Engine, telemetry::*};
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

struct Case {
    family: &'static str,
    name: String,
    recovery: corpus::Recovery,
}

#[tokio::test]
#[ignore = "exports private conformance evidence only when an explicit output path is supplied"]
async fn export_vendor_field_conformance_from_canonical_original_evidence() {
    let output = PathBuf::from(std::env::var_os("CULPRIT_VENDOR_CONFORMANCE_OUTPUT").expect("explicit private output"));
    assert!(output.is_absolute(), "the output must name an absolute private artifact");
    let catalog = LogicalCatalog::new().unwrap();
    let mut cases = Vec::new();
    let mut sequence = 10;
    for (family, name, source, rows) in inputs() {
        // Shared corpora also contain other providers. Preserve them as negative controls; they cannot
        // establish coverage for a Microsoft field, whose verifier also checks the original engine.
        let family = if source.target.engine() == Engine::Mssql { family } else { "foreign-source-controls" };
        let original = rows.clone();
        let recovery = corpus::Recovery::capture("field-conformance", catalog.digest(), source, rows, sequence);
        sequence += 1;
        cases.push(export_case(Case { family, name, recovery }, Some(&original), &catalog).await);
    }
    // Existing interval fixtures already own their original query source, counter meanings and receipts.
    for (name, recovery) in [
        ("interval", corpus::interval_queries("field-conformance", catalog.digest())),
        ("cumulative", corpus::cumulative_queries("field-conformance", catalog.digest())),
        ("wait-exact", corpus::waits("field-conformance", catalog.digest(), 10_000, false, true)),
    ] {
        cases.push(
            export_case(
                Case { family: "additional-original-counter-controls", name: name.into(), recovery },
                None,
                &catalog,
            )
            .await,
        );
    }
    let schemas = catalog
        .schemas()
        .map(|(stream, schema)| {
            json!({
                "stream":stream, "table":schema.table(), "digest":schema.digest(), "fields":schema.fields(),
            })
        })
        .collect::<Vec<_>>();
    let sources = culprit_mssql::sources::ALL
        .iter()
        .map(|source| {
            json!({
                "id":source.name, "description_key":source.reads, "delivers":source.delivers,
            })
        })
        .collect::<Vec<_>>();
    let profile = Engine::Mssql.spec().performance_counters().unwrap();
    let counters = profile
        .fields
        .iter()
        .map(|field| {
            let instances = match field.instances {
                PerformanceCounterInstances::Uninstanced => json!({"kind":"uninstanced"}),
                PerformanceCounterInstances::Named(names) => json!({"kind":"named", "names":names}),
                PerformanceCounterInstances::Any => json!({"kind":"any"}),
            };
            json!({"id":field.id, "native_object_family":field.object_family, "native_name":field.native_name,
            "instances":instances, "unit":field.unit, "nature":field.nature, "family":field.family,
            "ratio":field.ratio, "application_projection":field.application})
        })
        .collect::<Vec<_>>();
    let value = json!({
        "conformance_export_version":1,
        "evidence_kind":"synthetic-existing-typed-corpus-through-original-recovery-and-shared-projection",
        "native_acquisition_executed":false,
        "native_cost_qualified":false,
        "source_catalog_sha256":digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/../providers/culprit-mssql/src/sources.rs"))),
        "recipe_catalog_sha256":digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/../providers/culprit-mssql/src/telemetry/recipes.rs"))),
        "schema_digest":catalog.digest(), "schemas":schemas, "sources":sources,
        "recipes":culprit_mssql::telemetry_recipes(),
        "performance_counter_profile":{"native_object":profile.native_object, "selection":profile.selection, "fields":counters},
        "cases":cases,
    });
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output).expect("exclusive private artifact creation");
    serde_json::to_writer_pretty(&mut file, &value).unwrap();
    file.write_all(b"\n").unwrap();
    eprintln!(
        "exported {} canonical corpus cases; no native acquisition or qualification",
        value["cases"].as_array().unwrap().len()
    );
}

async fn export_case(case: Case, expected: Option<&[TelemetryRecord]>, catalog: &LogicalCatalog) -> Value {
    let mut rows = Vec::new();
    let mut originals = Vec::new();
    let manifest = &case.recovery.manifest;
    manifest.validate().unwrap();
    for batch in &manifest.batches {
        let schema = catalog.get(batch.stream).unwrap();
        let envelope = Envelope::new(manifest, &batch.batch_id).unwrap();
        let mut reader = RecoveryRecords::open(&case.recovery, batch).await.unwrap();
        let mut ordinal = 0;
        while let Some(record) = reader.next().await.unwrap() {
            ordinal += 1;
            assert_eq!(
                record,
                ExportedRecord::new(&manifest.source, &manifest.capture, record.record.clone()).unwrap()
            );
            let prepared = envelope.input(ordinal, &record).unwrap();
            let projected = schema.project(&prepared).unwrap();
            let logical = schema
                .fields()
                .iter()
                .zip(&projected)
                .map(|(field, value)| (field.name.to_owned(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            let wire: Value = serde_json::from_slice(&schema.encode(&projected).unwrap()).unwrap();
            let resource_wait_time_ms = match &record.record {
                TelemetryRecord::WaitCounter(wait) => wait.resource_wait_time_ms(),
                _ => None,
            };
            let payload_pointers =
                record.record.definition_payloads().map(|field| field.pointer.to_string()).collect::<Vec<_>>();
            rows.push(json!({"original":record, "logical":logical, "warehouse":wire,
                "derived_resource_wait_ms":resource_wait_time_ms, "definition_payload_pointers":payload_pointers}));
            originals.push(record.record);
        }
        assert_eq!(ordinal, batch.rows.len(), "EOF verified the complete byte/hash/count receipt");
    }
    if let Some(expected) = expected {
        let mut wanted = expected
            .iter()
            .map(|row| {
                let canonical = ExportedRecord::new(&manifest.source, &manifest.capture, row.clone()).unwrap();
                serde_json::to_string(&canonical.record).unwrap()
            })
            .collect::<Vec<_>>();
        let mut actual = originals.iter().map(|row| serde_json::to_string(row).unwrap()).collect::<Vec<_>>();
        wanted.sort();
        actual.sort();
        assert_eq!(actual, wanted, "no missing/extra original corpus members after canonical recovery");
    }
    json!({"family":case.family, "name":case.name, "manifest":manifest, "rows":rows})
}

fn inputs() -> Vec<(&'static str, String, CollectionDescription, Vec<TelemetryRecord>)> {
    let mut cases = Vec::new();
    for case in corpus::DatabasePropertiesCase::ALL {
        let (s, r) = corpus::database_properties_evidence(case);
        cases.push(("database-properties", format!("{case:?}"), s, r));
    }
    for case in corpus::BackupHistoryCase::ALL {
        let (s, r) = corpus::backup_history_evidence(case);
        cases.push(("backup-history", format!("{case:?}"), s, r));
    }
    for case in corpus::CdcCase::ALL {
        let (s, r) = corpus::cdc_evidence(case);
        cases.push(("cdc", format!("{case:?}"), s, r));
    }
    for case in corpus::MemoryClerkCase::ALL {
        let (s, r) = corpus::memory_clerk_evidence(case);
        cases.push(("memory-clerks", format!("{case:?}"), s, r));
    }
    for case in corpus::PerformanceCounterCase::ALL {
        let (s, r) = corpus::performance_counter_evidence(case);
        cases.push(("performance-counters", format!("{case:?}"), s, r));
    }
    for case in corpus::InstanceResourceCase::ALL {
        let (s, r) = corpus::instance_resource_evidence(case);
        cases.push(("instance-resources", format!("{case:?}"), s, r));
    }
    for case in corpus::WaitCounterCase::ALL {
        let (s, r) = corpus::wait_counter_evidence(case);
        cases.push(("wait-counters", format!("{case:?}"), s, r));
    }
    for case in corpus::SessionCensusCase::ALL {
        let (s, r) = corpus::session_census_evidence(case);
        cases.push(("session-census", format!("{case:?}"), s, r));
    }
    for case in corpus::StorageCase::ALL {
        let (s, r) = corpus::storage_evidence(case);
        cases.push(("storage", format!("{case:?}"), s, r));
    }
    for case in corpus::TableFullTextCase::ALL {
        let (s, r) = corpus::table_full_text_evidence(case);
        cases.push(("table-catalog", format!("{case:?}"), s, r));
    }
    for (case, source, records) in corpus::tuning_evidence_cases() {
        cases.push(("tuning", format!("{case:?}"), source, records));
    }
    for case in corpus::MssqlAgentJobsCase::ALL {
        let (s, r) = corpus::mssql_agent_jobs_evidence(*case);
        cases.push(("agent-jobs", format!("{case:?}"), s, r));
    }
    for (family, values) in [
        ("activity", corpus::activity_evidence_cases()),
        ("file-io", corpus::file_io_evidence_cases()),
        ("columnstore-index-catalog", corpus::columnstore_configuration_evidence_cases()),
        ("replication", corpus::replication_evidence_cases()),
        ("resource-role", corpus::resource_role_evidence_cases()),
        ("query-waits", corpus::query_wait_evidence_cases()),
    ] {
        for (index, (source, records)) in values.into_iter().enumerate() {
            if source.target.engine() == Engine::Mssql {
                cases.push((family, index.to_string(), source, records));
            }
        }
    }
    for (family, (source, records)) in
        [("oom", corpus::memory_failure_evidence()), ("query-runtime", corpus::boxed_query_history_evidence())]
    {
        cases.push((family, "original".into(), source, records));
    }
    let (source, record) = corpus::pool_history();
    cases.push(("pool-resource-history", "original".into(), source, vec![record]));
    cases
}
