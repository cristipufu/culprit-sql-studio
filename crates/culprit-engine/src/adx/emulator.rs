//! Functional query/mapping correctness only. The Kusto emulator is never used for capacity or performance tests.
use super::{
    catalog::SchemaCatalog,
    client::{self, QueryRequest, QueryTransport},
    deployment::DeploymentAssets,
    response::{Column, QueryTable},
    sink::AdxTelemetrySink,
    test_support::{self, Blobs, Recovery},
};
use async_trait::async_trait;
use culprit_collector::delivery::{
    recovery::{CommittedRange, reopen},
    sink::*,
};
use culprit_core::{Result, telemetry::*};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

struct Emulator {
    port: u16,
}
impl Emulator {
    fn management(&self, database: bool, text: &str) {
        let mut body = json!({"csl":text});
        if database {
            body["db"] = json!("CulpritFixture");
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(45)))
            .build()
            .into();
        let mut response = agent
            .post(format!("http://127.0.0.1:{}/v1/rest/mgmt", self.port))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .expect("owned emulator management request");
        let bytes = response
            .body_mut()
            .with_config()
            .limit(16 * 1024 * 1024)
            .read_to_vec()
            .expect("complete emulator management reply");
        let reply: Value = serde_json::from_slice(&bytes).expect("emulator management JSON");
        assert!(reply.get("error").is_none() && reply.get("Exceptions").is_none(), "{reply}");
    }
    fn ingest(&self, table: &str, bytes: &[u8]) {
        let text = std::str::from_utf8(bytes).unwrap();
        self.management(true,&format!(".ingest inline into table {table} with (format=\"json\",ingestionMappingReference=\"{table}Mapping\") <|\n{text}"));
    }
    async fn values(&self, text: String, parameters: BTreeMap<String, Value>, columns: Vec<Column>) -> Vec<Vec<Value>> {
        self.query(QueryRequest { text, parameters, columns, row_limit: 10_000 }).await.unwrap().rows
    }
    async fn count(&self, function: &str, range: &CommittedRange, current: bool) -> u64 {
        let clock =
            if current { "datetime(2020-01-01T12:05:00Z),1h" } else { "datetime(2019-01-01),datetime(2021-01-01)" };
        let text = format!(
            "declare query_parameters(c:string,s:string,d:string); {function}(c,s,d,{clock}) | summarize N=count()"
        );
        self.values(text, scope(range), vec![Column { column_name: "N".into(), column_type: "long".into() }]).await[0]
            [0]
        .as_u64()
        .unwrap()
    }
}
#[async_trait]
impl QueryTransport for Emulator {
    fn destination(&self) -> &str {
        "adx:test"
    }
    async fn query(&self, request: QueryRequest) -> Result<QueryTable> {
        client::emulator_query(self.port, request)
    }
}
fn scope(range: &CommittedRange) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("c".into(), json!(range.manifest().contract.id().unwrap())),
        ("s".into(), json!(range.store_id())),
        ("d".into(), json!(range.manifest().source.database.telemetry_id().unwrap())),
    ])
}
struct Scenario {
    _scratch: tempfile::TempDir,
    range: CommittedRange,
    sink: AdxTelemetrySink,
    blobs: Arc<Blobs>,
    ingested: Mutex<BTreeSet<String>>,
}
impl Scenario {
    async fn new(recovery: Recovery, emulator: Arc<Emulator>) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let recovery = Arc::new(recovery);
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let blobs = Arc::new(Blobs::default());
        let sink =
            AdxTelemetrySink::new(range.manifest().contract.clone(), recovery, blobs.clone(), emulator, scratch.path())
                .unwrap();
        Self { _scratch: scratch, range, sink, blobs, ingested: Mutex::new(BTreeSet::new()) }
    }
    async fn dispatch(&self, emulator: &Emulator) {
        dispatch_range(&self.sink, &self.range, &DeliveryAttempt::new(1).unwrap()).await.unwrap();
        self.ingest(emulator);
    }
    fn ingest(&self, emulator: &Emulator) {
        let objects = self.blobs.objects.lock().unwrap();
        let mut ingested = self.ingested.lock().unwrap();
        for (key, bytes) in objects.iter().filter(|(key, _)| key.starts_with("ready/")) {
            if ingested.insert(key.clone()) {
                emulator.ingest(key.split('/').nth(1).unwrap(), bytes);
            }
        }
    }
    async fn publish(&self, emulator: &Emulator) {
        let verified = verify(&self.sink, &self.range, VerificationBudget::default()).await.unwrap();
        let publication = self.sink.publish(&verified, &DeliveryAttempt::new(2).unwrap()).await.unwrap();
        assert!(!self.sink.publication_visible(&publication, &self.range).await.unwrap());
        self.ingest(emulator);
        match self.sink.publication_visible(&publication, &self.range).await {
            Ok(visible) => assert!(visible),
            Err(error) => {
                self.describe_publication(emulator).await;
                panic!("synthetic publication readback failed: {error:?}");
            }
        }
    }
    async fn describe_publication(&self, emulator: &Emulator) {
        let catalog = SchemaCatalog::new().unwrap();
        let schema = catalog.publication();
        let stored = {
            let objects = self.blobs.objects.lock().unwrap();
            serde_json::from_slice::<Value>(
                objects.iter().find(|(key, _)| key.starts_with("ready/CulpritPublishedRangesV1/")).unwrap().1,
            )
            .unwrap()
        };
        let text = format!(
            "declare query_parameters(pRange:string); {} | where RangeId == pRange | project {} | take 1",
            schema.table(),
            schema.verification_projection()
        );
        let rows = emulator
            .values(
                text,
                BTreeMap::from([("pRange".into(), json!(self.range.manifest().range_id))]),
                schema.verification_columns(),
            )
            .await;
        for (column, actual) in schema.columns().iter().zip(&rows[0]) {
            let expected = column.kind.returned(&stored[&column.name], column.nullable).unwrap();
            let normalized = column.kind.returned(actual, column.nullable);
            if normalized.as_ref().ok() != Some(&expected) {
                // This test uses synthetic fixtures only; retain useful readback evidence without changing
                // production errors or relaxing any membership/value verification.
                eprintln!(
                    "Synthetic control column {}: expected={expected:?}, returned={actual:?}, normalized={normalized:?}",
                    column.name
                );
            }
        }
    }
}
fn activity(catalog: &SchemaCatalog, sequence: u64, empty: bool) -> Recovery {
    let (mut manifest, _) = test_support::fixture();
    manifest.source.source = "activity".into();
    manifest.source.observation = SourceObservation::Activity(ActivitySnapshotDescription {
        epoch: "native-epoch".into(),
        at: "2020-01-01T12:00:00Z".into(),
        database_wide: true,
        policy: ActivityPolicy::default(),
        native_rows: u32::from(!empty),
        qualifying: u32::from(!empty),
        observations: u32::from(!empty),
        blocking_edges: 0,
        details: ActivityDetailStatus::Collected,
    });
    let record = TelemetryRecord::Activity(ActivityObservation {
        key: ActivityKey {
            session: NativeActivityIdentity { id: "61".into(), generation: "login-1".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-1".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        database: Some("db".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: 0,
        elapsed_ms: Some("120000".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters::default()),
        wait: None,
        query_hash: None,
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    });
    Recovery::capture(catalog, manifest.source, if empty { vec![] } else { vec![record] }, sequence)
}

fn waits(catalog: &SchemaCatalog, sequence: u64, empty: bool, complete: bool) -> Recovery {
    let (mut manifest, _) = test_support::fixture();
    manifest.source.source = "wait-counters".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::WaitCounters,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: u32::from(!empty),
        sources: vec![StateSourceCoverage::observed("native-waits".into(), u32::from(!empty), complete)],
    });
    let row = TelemetryRecord::WaitCounter(WaitCounterObservation {
        source: "native-waits".into(),
        wait_type: "LCK_M_X".into(),
        scope: culprit_core::MeasurementScope::Database,
        reset: culprit_core::ResetMarker::Unknown,
        waiting_tasks_count: "9007199254740993".into(),
        wait_time_ms: "18446744073709551615".into(),
        signal_wait_time_ms: "0".into(),
        max_wait_time_ms: "7".into(),
    });
    Recovery::capture(catalog, manifest.source, if empty { vec![] } else { vec![row] }, sequence)
}

#[tokio::test]
#[ignore = "Run only through dev/smoke-adx.py on its owned x86 Linux emulator; no Azure/ADX resources are used."]
async fn functional_contract_and_canonical_sets() {
    let port = std::env::var("CULPRIT_TEST_KUSTO_PORT").expect("owned emulator port").parse::<u16>().unwrap();
    assert!(port > 0);
    let emulator = Arc::new(Emulator { port });
    emulator.management(false,".create database CulpritFixture persist (@\"/kustodata/dbs/CulpritFixture/md\", @\"/kustodata/dbs/CulpritFixture/data\")");
    for command in DeploymentAssets::new().unwrap().commands {
        emulator.management(true, &command);
    }
    let catalog = SchemaCatalog::new().unwrap();
    let first = Scenario::new(Recovery::new(&catalog), emulator.clone()).await;
    first.dispatch(&emulator).await;
    assert_eq!(emulator.count("CulpritResourceSamplesV1Rows", &first.range, false).await, 0);
    first.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritResourceSamplesV1Rows", &first.range, false).await, 1);
    // Repeated native data delivery cannot multiply totals.
    {
        let objects = first.blobs.objects.lock().unwrap();
        let bytes = objects.iter().find(|(key, _)| key.starts_with("ready/CulpritResourceSamplesV1/")).unwrap().1;
        emulator.ingest("CulpritResourceSamplesV1", bytes);
    }
    assert_eq!(emulator.count("CulpritResourceSamplesV1Rows", &first.range, false).await, 1);
    let (manifest, mut row) = test_support::fixture();
    let TelemetryRecord::ResourceSample(sample) = &mut row.record else { unreachable!() };
    sample.metrics[0].value = Some(30.0);
    let later =
        Scenario::new(Recovery::capture(&catalog, manifest.source, vec![row.record], 3), emulator.clone()).await;
    later.dispatch(&emulator).await;
    // A new unpublished observation cannot hide the earlier published measurement.
    let query = "declare query_parameters(c:string,s:string,d:string); CulpritResourceSamplesV1Rows(c,s,d,datetime(2019),datetime(2021)) | project Cpu=todouble(Metrics[0].value)";
    let columns = vec![Column { column_name: "Cpu".into(), column_type: "real".into() }];
    assert_eq!(emulator.values(query.into(), scope(&first.range), columns.clone()).await, vec![vec![json!(12.5)]]);
    later.publish(&emulator).await;
    assert_eq!(emulator.values(query.into(), scope(&first.range), columns.clone()).await, vec![vec![json!(30.0)]]);
    let active = Scenario::new(activity(&catalog, 4, false), emulator.clone()).await;
    active.dispatch(&emulator).await;
    active.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 1);
    let mut restarted = activity(&catalog, 5, true);
    let SourceObservation::Activity(snapshot) = &mut restarted.manifest.source.observation else { unreachable!() };
    snapshot.epoch = "restarted-native-epoch".into();
    let empty = Scenario::new(restarted, emulator.clone()).await;
    empty.dispatch(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 1);
    empty.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 0);
    let counter = Scenario::new(waits(&catalog, 6, false, true), emulator.clone()).await;
    counter.dispatch(&emulator).await;
    counter.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritWaitCountersV1Current", &counter.range, true).await, 1);
    let text = "declare query_parameters(c:string,s:string,d:string); CulpritWaitCountersV1Current(c,s,d,datetime(2020-01-01T12:05:00Z),1h) | project Tasks=tostring(WaitingTasks), Ms=tostring(WaitTimeMs), ResetKind";
    let exact_columns = ["Tasks", "Ms", "ResetKind"]
        .into_iter()
        .map(|name| Column { column_name: name.into(), column_type: "string".into() })
        .collect();
    assert_eq!(
        emulator.values(text.into(), scope(&counter.range), exact_columns).await,
        vec![vec![json!("9007199254740993"), json!("18446744073709551615"), json!("unknown")]]
    );
    let partial = Scenario::new(waits(&catalog, 7, true, false), emulator.clone()).await;
    partial.dispatch(&emulator).await;
    partial.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritWaitCountersV1Current", &counter.range, true).await, 1);
    let cleared = Scenario::new(waits(&catalog, 8, true, true), emulator.clone()).await;
    cleared.dispatch(&emulator).await;
    cleared.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritWaitCountersV1Current", &counter.range, true).await, 0);
    // Unexpected late landing members invalidate that capture rather than changing trusted totals.
    {
        let objects = later.blobs.objects.lock().unwrap();
        let mut row: Value = serde_json::from_slice(
            objects.iter().find(|(key, _)| key.starts_with("ready/CulpritResourceSamplesV1/")).unwrap().1,
        )
        .unwrap();
        row["RowOrdinal"] = json!(99);
        emulator.ingest("CulpritResourceSamplesV1", &serde_json::to_vec(&row).unwrap());
    }
    assert!(verify(&later.sink, &later.range, VerificationBudget::default()).await.is_err());
    assert_eq!(emulator.values(query.into(), scope(&first.range), columns).await, vec![vec![json!(12.5)]]);
}
