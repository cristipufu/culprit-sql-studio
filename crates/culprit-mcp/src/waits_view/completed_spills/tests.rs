use super::*;
use culprit_core::{DatabaseIdentity, Engine, ErrorCode, SourceState};

const SOURCE: &str = "private-completed-spill-source-canary";
const EXACT_COUNT: &str = "9007199254740993";
const FROM: &str = "2026-11-13T10:00:00.000000001Z";
const UNTIL: &str = "2026-11-13T10:00:01.000000002Z";

fn data(count: usize) -> Box<Report> {
    let mut data: Box<Report> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let mut database =
        DatabaseIdentity::server(Engine::Mssql, "owned-spill-db", "private-spill-route.invalid", 1433, None);
    database.incarnation = Some("private-spill-database-incarnation".into());
    data.snapshot.server.database = database.database.clone();
    data.snapshot.bind_identity(database.clone());
    let observations = (0..count)
        .map(|index| {
            let point = CachedStatementRuntime {
                context: CachedStatementContext {
                    identity: CachedStatementIdentity {
                        plan_handle: "0xaabbccdd".into(),
                        sql_handle: "0xeeff0011".into(),
                        statement_start_bytes: 0,
                        statement_end_bytes: -1,
                        created_without_offset: "2026-11-13T09:00:00.003".into(),
                        plan_generation: (index + 1).to_string(),
                    },
                    database_id: 7,
                    query_hash: Some("0x0123456789ABCDEF".into()),
                    origin: StatementOrigin::Unknown,
                    monitoring_template_id: None,
                    source_text_bytes: None,
                    source_text_complete: false,
                },
                query_plan_hash: None,
                executions: Some("17".into()),
                counters: CachedRuntimeMetric::ALL
                    .into_iter()
                    .map(|metric| {
                        (
                            metric,
                            CachedCounterDistribution {
                                total: EXACT_COUNT.into(),
                                last: "3".into(),
                                minimum: "0".into(),
                                maximum: "4".into(),
                            },
                        )
                    })
                    .collect(),
                last_execution_without_offset: Some("2026-11-13T09:59:00.003".into()),
                reset: culprit_core::ResetMarker::Generation("private-spill-reset-canary".into()),
            };
            OperationalObservation {
                source: SOURCE.into(),
                key: point.context.identity.key().unwrap(),
                fact: OperationalFact::MssqlCachedRuntime(Box::new(point)),
            }
        })
        .collect();
    let read = CachedRuntimeRead {
        read_id: cached_statement_read_id(&CaptureIdentity { observation_id: "a".repeat(32), revision: None }).unwrap(),
        read_complete: true,
        description: CollectionDescription {
            target: database.into(),
            source: "private-spill-logical-source-canary".into(),
            continuity: SourceContinuity::Unverified,
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("private-spill-machine"),
                    native: NativeIdentity::new("private-spill-member"),
                }),
                epoch: Some("private-spill-process".into()),
            },
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::CachedRuntime,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: count as u32,
                sources: vec![StateSourceCoverage::collected(SOURCE.into(), count as u32)],
            }),
        },
        observations,
    };
    assert!(read.is_valid());
    data.snapshot.operations.cached_runtime = Some(Box::new(read));
    data
}

fn compact(data: &Report) -> Value {
    let mut response = json!({});
    attach(&mut response, data);
    response
}

fn header(data: &mut Report) -> &mut StateSnapshotDescription {
    let read = data.snapshot.operations.cached_runtime.as_deref_mut().unwrap();
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header
}

#[test]
fn compact_native_read_keeps_exact_counts_clocks_and_opaque_positions_with_five_entries() {
    let data = data(ENTRY_LIMIT + 3);
    let original = serde_json::to_vec(&data.snapshot.operations.cached_runtime).unwrap();
    let response = super::super::report(&data);
    let view = &response["completed_spills"];
    assert_eq!(view["entries_retained"], ENTRY_LIMIT + 3);
    assert_eq!(view["entries_presented"], ENTRY_LIMIT);
    assert_eq!(view["entries_limited"], true);
    assert_eq!(view["entries"].as_array().unwrap().len(), ENTRY_LIMIT);
    assert_eq!(view["native_rows"], ENTRY_LIMIT + 3);
    assert_eq!(view["observed_rows"], ENTRY_LIMIT + 3);
    assert_eq!(view["read_complete"], true);
    assert_eq!(view["read_bounds"]["observed_from"], FROM);
    assert_eq!(view["read_bounds"]["observed_until"], UNTIL);
    assert_eq!(view["entries"][0]["metrics"][0]["text"], EXACT_COUNT);
    assert_eq!(view["entries"][0]["reference"]["kind"], "native-read");
    assert_eq!(view["entries"][0]["statement_binding"], "not-requested");
    assert!(view["definition_read_complete"].is_null());
    assert!(view["definition_read_bounds"].is_null());
    assert!(response["operations"].get("cached_runtime").is_none());
    let serialized = serde_json::to_string(view).unwrap();
    for private in [SOURCE, "private-spill-", "0xaabbccdd", "0xeeff0011"] {
        assert!(!serialized.contains(private), "raw native field leaked: {private}");
    }
    for field in ["identity", "context", "reset", "statement_preview", "database_id", "snapshot", "observations"] {
        assert!(view["entries"][0].get(field).is_none(), "raw {field} must stay outside compact entries");
    }
    assert_eq!(original, serde_json::to_vec(&data.snapshot.operations.cached_runtime).unwrap());
    assert!(serialized.len() < 20_000);
}

#[test]
fn fully_read_partial_native_population_is_distinct_from_a_missing_warehouse_body() {
    let mut native_partial = data(1);
    header(&mut native_partial).sources[0] = StateSourceCoverage::observed(SOURCE.into(), 1, false);
    let native = compact(&native_partial);
    assert_eq!(native["completed_spills"]["read_complete"], true);
    assert_eq!(native["completed_spills"]["native_rows"], 1);
    assert_eq!(native["completed_spills"]["sources"][0]["state"], "partial");
    let mut body_partial = data(1);
    header(&mut body_partial).records = 3;
    header(&mut body_partial).sources[0] = StateSourceCoverage::collected(SOURCE.into(), 3);
    body_partial.snapshot.operations.cached_runtime.as_deref_mut().unwrap().read_complete = false;
    let body = compact(&body_partial);
    assert_eq!(body["completed_spills"]["read_complete"], false);
    assert_eq!(body["completed_spills"]["native_rows"], 3);
    assert_eq!(body["completed_spills"]["observed_rows"], 1);
    assert_eq!(body["completed_spills"]["sources"][0]["state"], "partial");
    assert!(
        body["completed_spills"]["limitations"]
            .as_array()
            .unwrap()
            .contains(&json!(culprit_core::t!("view.completed_spills.limit.partial")))
    );
}

#[test]
fn empty_unavailable_and_missing_counter_reads_never_become_a_healthy_zero() {
    let empty = compact(&data(0));
    assert_eq!(empty["completed_spills"]["entries"], json!([]));
    assert_eq!(empty["completed_spills"]["sources"][0]["state"], "empty");
    assert_eq!(empty["completed_spills"]["native_rows"], 0);
    let mut unavailable = data(0);
    header(&mut unavailable).sources[0] =
        StateSourceCoverage::unavailable(SOURCE.into(), ErrorCode::PermissionDenied, None);
    let value = compact(&unavailable);
    assert_eq!(value["completed_spills"]["sources"][0]["state"], "unavailable");
    assert_eq!(value["completed_spills"]["entries"], json!([]));
    let mut zero = data(1);
    let row = &mut zero.snapshot.operations.cached_runtime.as_deref_mut().unwrap().observations[0];
    let OperationalFact::MssqlCachedRuntime(sample) = &mut row.fact else { panic!() };
    sample.counters.insert(
        CachedRuntimeMetric::SpilledPages,
        CachedCounterDistribution { total: "0".into(), last: "0".into(), minimum: "0".into(), maximum: "0".into() },
    );
    let value = compact(&zero);
    assert_eq!(value["completed_spills"]["entries"][0]["state"], "no-recorded-spills");
    assert_eq!(value["completed_spills"]["entries"][0]["metrics"][0]["text"], "0");
    let mut missing = data(1);
    let row = &mut missing.snapshot.operations.cached_runtime.as_deref_mut().unwrap().observations[0];
    let OperationalFact::MssqlCachedRuntime(sample) = &mut row.fact else { panic!() };
    sample.counters.remove(&CachedRuntimeMetric::SpilledPages);
    let value = compact(&missing);
    assert_eq!(value["completed_spills"]["entries"][0]["state"], "unavailable");
    assert!(
        value["completed_spills"]["entries"][0]["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|metric| metric["key"] != "view.completed_spills.metric.total")
    );
}

#[test]
fn raw_carrier_admission_rejects_foreign_scope_duplicates_and_stale_derived_reports() {
    let mut foreign = data(1);
    foreign.completed_spills = culprit_engine::explain_completed_spills(&foreign.snapshot).unwrap().map(Box::new);
    let CollectionTarget::Database(database) =
        &mut foreign.snapshot.operations.cached_runtime.as_deref_mut().unwrap().description.target
    else {
        panic!()
    };
    database.database = "other-database".into();
    let mut duplicate = data(2);
    let read = duplicate.snapshot.operations.cached_runtime.as_deref_mut().unwrap();
    read.observations[1] = read.observations[0].clone();
    let mut derived_only = data(1);
    derived_only.completed_spills =
        culprit_engine::explain_completed_spills(&derived_only.snapshot).unwrap().map(Box::new);
    derived_only.snapshot.operations.cached_runtime = None;
    for input in [&foreign, &duplicate, &derived_only] {
        let value = compact(input);
        assert!(value.get("completed_spills").is_none());
        assert_eq!(value["completed_spills_unavailable"]["reason"], culprit_core::t!("view.completed_spills.invalid"));
        assert!(!serde_json::to_string(&value).unwrap().contains(EXACT_COUNT));
    }
}

#[test]
fn valid_native_input_is_explained_once_without_trusting_a_stale_precomputed_view() {
    let mut data = data(1);
    let mut stale = culprit_engine::explain_completed_spills(&data.snapshot).unwrap().unwrap();
    stale.summary = "stale-derived-summary-canary".into();
    stale.entries[0].metrics[0].text = Some("42".into());
    data.completed_spills = Some(Box::new(stale));
    let value = compact(&data);
    assert_eq!(value["completed_spills"]["entries"][0]["metrics"][0]["text"], EXACT_COUNT);
    assert!(!serde_json::to_string(&value).unwrap().contains("stale-derived-summary-canary"));
}

#[test]
fn a_source_only_gap_is_explicit_and_does_not_render_private_reason_text() {
    let mut data = data(0);
    data.snapshot.operations.cached_runtime = None;
    data.snapshot.sources = vec![SourceOutcome {
        source: CACHED_RUNTIME_READ_SOURCE.into(),
        state: SourceState::Unavailable,
        reason: Some("private-permission-query-canary".into()),
        reason_message: None,
    }];
    let value = compact(&data);
    assert_eq!(value["completed_spills_unavailable"]["sources"][0]["state"], "unavailable");
    assert!(!serde_json::to_string(&value).unwrap().contains("private-permission-query-canary"));
    data.snapshot.sources.clear();
    assert_eq!(compact(&data), json!({}), "an unrequested category is not a fabricated failure");
}

#[test]
fn captured_and_native_positions_stay_distinct_and_neither_exposes_sql_or_native_receipt_fields() {
    let data = data(1);
    let read = data.snapshot.operations.cached_runtime.as_deref().unwrap();
    let source = &read.description;
    let capture = CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let record =
        ExportedRecord::new(source, &capture, TelemetryRecord::Operational(read.observations[0].clone())).unwrap();
    let receipt = DiagnosticEvidenceRef::from_export(source, &digest(b"original-manifest"), &record, 0).unwrap();
    receipt.validate_canonical_record(source, &record.record).unwrap();
    let mut entry = culprit_engine::explain_completed_spills(&data.snapshot).unwrap().unwrap().entries.remove(0);
    let native = point(&entry).unwrap();
    entry.observation = CompletedSpillReference::Captured { reference: Box::new(receipt.clone()) };
    entry.statement_preview = Some("SELECT private_spill_sql_canary".into());
    let captured = point(&entry).unwrap();
    assert_eq!(captured["reference"], json!({ "kind": "captured", "reference": receipt.key().unwrap() }));
    assert_ne!(captured["reference"], native["reference"]);
    let body = serde_json::to_string(&captured).unwrap();
    for private in [SOURCE, "private_spill_sql_canary", "native_key", "manifest_sha256", "outbox_id"] {
        assert!(!body.contains(private));
    }
    let mut changed = receipt;
    changed.manifest_sha256 = digest(b"another-original-manifest");
    assert_ne!(
        reference(&CompletedSpillReference::Captured { reference: Box::new(changed) }).unwrap(),
        captured["reference"]
    );
}
