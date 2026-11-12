//! Functional query/mapping correctness only. The Kusto emulator is never used for capacity or performance tests.
use super::{
    catalog::SchemaCatalog,
    client::{self, QueryRequest, QueryTransport},
    deployment::DeploymentAssets,
    response::{Column, QueryTable},
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
    sink: culprit_collector::analytical::sink::AnalyticalSink,
    blobs: Arc<Blobs>,
    ingested: Mutex<BTreeSet<String>>,
}
impl Scenario {
    async fn new(recovery: Recovery, emulator: Arc<Emulator>) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let recovery = Arc::new(recovery);
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let blobs = Arc::new(Blobs::default());
        let sink = crate::test_support::event_grid_sink(
            range.manifest().contract.clone(),
            recovery,
            blobs.clone(),
            emulator,
            scratch.path(),
        )
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
    let first = Scenario::new(Recovery::new("adx:test", catalog.digest()), emulator.clone()).await;
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
    let later = Scenario::new(
        Recovery::capture("adx:test", catalog.digest(), manifest.source, vec![row.record], 3),
        emulator.clone(),
    )
    .await;
    later.dispatch(&emulator).await;
    // A new unpublished observation cannot hide the earlier published measurement.
    let query = "declare query_parameters(c:string,s:string,d:string); CulpritResourceSamplesV1Rows(c,s,d,datetime(2019),datetime(2021)) | project Cpu=todouble(Metrics[0].value)";
    let columns = vec![Column { column_name: "Cpu".into(), column_type: "real".into() }];
    assert_eq!(emulator.values(query.into(), scope(&first.range), columns.clone()).await, vec![vec![json!(12.5)]]);
    later.publish(&emulator).await;
    assert_eq!(emulator.values(query.into(), scope(&first.range), columns.clone()).await, vec![vec![json!(30.0)]]);
    let active = Scenario::new(test_support::activity("adx:test", catalog.digest(), 4, false), emulator.clone()).await;
    active.dispatch(&emulator).await;
    active.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 1);
    let mut restarted = test_support::activity("adx:test", catalog.digest(), 5, true);
    let SourceObservation::Activity(snapshot) = &mut restarted.manifest.source.observation else { unreachable!() };
    snapshot.epoch = "restarted-native-epoch".into();
    let empty = Scenario::new(restarted, emulator.clone()).await;
    empty.dispatch(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 1);
    empty.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritActivityV1Current", &active.range, true).await, 0);
    let counter =
        Scenario::new(test_support::waits("adx:test", catalog.digest(), 6, false, true), emulator.clone()).await;
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
    let partial =
        Scenario::new(test_support::waits("adx:test", catalog.digest(), 7, true, false), emulator.clone()).await;
    partial.dispatch(&emulator).await;
    partial.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritWaitCountersV1Current", &counter.range, true).await, 1);
    let cleared =
        Scenario::new(test_support::waits("adx:test", catalog.digest(), 8, true, true), emulator.clone()).await;
    cleared.dispatch(&emulator).await;
    cleared.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritWaitCountersV1Current", &counter.range, true).await, 0);
    let history = Scenario::new(test_support::interval_queries("adx:test", catalog.digest()), emulator.clone()).await;
    history.dispatch(&emulator).await;
    history.publish(&emulator).await;
    assert_eq!(emulator.count("CulpritQueryRuntimeV1Rows", &history.range, false).await, 1);
    let runtime_query = "declare query_parameters(c:string,s:string,d:string); CulpritQueryRuntimeV1Rows(c,s,d,datetime(2019),datetime(2021)) | project N=tostring(Executions), PageBytes=tolong(Metrics[1].unit.bytes_per_page), HasPlan=HasPlanDefinitionId";
    let runtime_columns = [("N", "string"), ("PageBytes", "long"), ("HasPlan", "bool")]
        .into_iter()
        .map(|(name, kind)| Column { column_name: name.into(), column_type: kind.into() })
        .collect();
    assert_eq!(
        emulator.values(runtime_query.into(), scope(&history.range), runtime_columns).await,
        vec![vec![json!("9007199254740993"), json!(16384), json!(false)]]
    );
    let wait_query = "declare query_parameters(c:string,s:string,d:string); CulpritQueryWaitsV1Rows(c,s,d,datetime(2019),datetime(2021)) | project TimeUnit, Total=todouble(TotalWait.value), HasAverage=isnotnull(AverageWait)";
    let wait_columns = [("TimeUnit", "string"), ("Total", "real"), ("HasAverage", "bool")]
        .into_iter()
        .map(|(name, kind)| Column { column_name: name.into(), column_type: kind.into() })
        .collect();
    assert_eq!(
        emulator.values(wait_query.into(), scope(&history.range), wait_columns).await,
        vec![vec![json!("microseconds"), json!(1.25), json!(false)]]
    );
    let cumulative =
        Scenario::new(test_support::cumulative_queries("adx:test", catalog.digest()), emulator.clone()).await;
    cumulative.dispatch(&emulator).await;
    cumulative.publish(&emulator).await;
    let cumulative_query = "declare query_parameters(c:string,s:string,d:string); CulpritOperationalV1Current(c,s,d,datetime(2020-01-01T12:05:00Z),1h) | project RangeKind, N=tostring(Details.totals[0].total.value), Reset=tostring(Details.reset.kind)";
    let cumulative_columns = ["RangeKind", "N", "Reset"]
        .into_iter()
        .map(|name| Column { column_name: name.into(), column_type: "string".into() })
        .collect();
    assert_eq!(
        emulator.values(cumulative_query.into(), scope(&cumulative.range), cumulative_columns).await,
        vec![vec![json!("current"), json!("9007199254740993"), json!("unknown")]]
    );
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
