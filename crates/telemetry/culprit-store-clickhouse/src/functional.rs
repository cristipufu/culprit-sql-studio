//! Native correctness against the owned loopback container created by dev/smoke-clickhouse.py.
use crate::{
    catalog::SchemaCatalog,
    client::{ClickHouseConfig, Credentials, QueryRequest, QueryTransport},
    deployment::DeploymentAssets,
    ingestion::{ClickHouseIngestor, IngestionTransport, WritePolicy},
    response::Column,
};
use culprit_collector::{
    analytical::{
        sink::AnalyticalSink,
        test_support::{self, Blobs, Fault, Recovery},
    },
    delivery::{
        recovery::{CommittedRange, reopen},
        sink::{DeliveryAttempt, TelemetrySink, VerificationBudget, dispatch_range, verify},
    },
};
use culprit_core::telemetry::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroizing;
const USER: &str = "culprit_fixture";
const PASSWORD: &str = "owned-fixture-password";
mod evidence;
mod waits;
struct Server {
    origin: String,
    client: Arc<dyn QueryTransport>,
    ingestion: Arc<ClickHouseIngestor>,
}
impl Server {
    fn new() -> Self {
        let port = std::env::var("CULPRIT_TEST_CLICKHOUSE_PORT")
            .expect("run through dev/smoke-clickhouse.py")
            .parse::<u16>()
            .unwrap();
        assert!(port > 0);
        let origin = format!("http://127.0.0.1:{port}");
        let ingestion = Arc::new(
            ClickHouseIngestor::new(
                ClickHouseConfig { endpoint: origin.clone(), database: "culprit_fixture".into() },
                Credentials::new(USER.into(), Zeroizing::new(PASSWORD.into())).unwrap(),
                WritePolicy::default(),
            )
            .unwrap(),
        );
        let client = ingestion.query_transport();
        Self { origin, client, ingestion }
    }
    fn execute(&self, sql: &str) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        let mut response = agent
            .post(format!("{}/", self.origin))
            .header("X-ClickHouse-User", USER)
            .header("X-ClickHouse-Key", PASSWORD)
            .query("database", "culprit_fixture")
            .send(format!("/* culprit */ {sql}"))
            .unwrap();
        let status = response.status().as_u16();
        let body = response.body_mut().with_config().limit(1024 * 1024).read_to_string().unwrap();
        assert_eq!(status, 200, "owned fixture SQL failed: {body}");
        assert!(body.trim().is_empty(), "owned fixture DDL failed: {body}");
    }
    async fn values(
        &self,
        text: String,
        parameters: BTreeMap<String, String>,
        columns: &[(&str, &str)],
    ) -> Vec<Vec<Value>> {
        self.client
            .query(QueryRequest {
                text,
                parameters,
                columns: columns
                    .iter()
                    .map(|(name, kind)| Column { name: (*name).into(), kind: (*kind).into() })
                    .collect(),
                row_limit: 10_000,
                byte_limit: crate::response::MAX_RESPONSE_BYTES,
            })
            .await
            .unwrap()
            .rows
    }
    fn scope(&self, range: &CommittedRange) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("contract".into(), range.manifest().contract.id().unwrap()),
            ("store".into(), range.store_id().into()),
            ("resource".into(), range.manifest().source.target.telemetry_id().unwrap()),
            ("from".into(), "2019-01-01 00:00:00".into()),
            ("until".into(), "2021-01-01 00:00:00".into()),
        ])
    }
    fn view(&self, name: &str, current: bool) -> String {
        let times = if current {
            "asof={asof:DateTime64(9, 'UTC')},max_age_ms={age:UInt64}"
        } else {
            "captured_from={from:DateTime64(9, 'UTC')},asof={until:DateTime64(9, 'UTC')}"
        };
        format!("{name}(contract={{contract:String}},store={{store:String}},resource={{resource:String}},{times})")
    }
    async fn count(&self, name: &str, range: &CommittedRange, current: bool) -> u64 {
        let mut parameters = self.scope(range);
        if current {
            parameters.remove("from");
            parameters.remove("until");
            parameters.insert("asof".into(), "2020-01-01 12:05:00".into());
            parameters.insert("age".into(), "3600000".into());
        }
        self.values(format!("SELECT count() AS n FROM {}", self.view(name, current)), parameters, &[("n", "UInt64")])
            .await[0][0]
            .as_u64()
            .unwrap()
    }
    fn deploy(&self) {
        for command in DeploymentAssets::new().unwrap().commands {
            self.execute(&command);
        }
        let catalog = SchemaCatalog::new().unwrap();
        for schema in catalog.schemas().map(|(_, schema)| schema).chain([catalog.publication()]) {
            // The host only supplies the disposable loopback fixture port. No database configuration is accepted.
            self.execute(&format!("TRUNCATE TABLE {}", crate::schema::quote(schema.table())));
        }
    }
}
struct Scenario {
    scratch: tempfile::TempDir,
    recovery: Arc<Recovery>,
    range: CommittedRange,
    sink: AnalyticalSink,
    indexes: Arc<Blobs>,
}
impl Scenario {
    async fn new(server: &Server, recovery: Recovery) -> Self {
        let recovery = Arc::new(recovery);
        let range = reopen(recovery.as_ref(), &recovery.manifest.range_id).await.unwrap();
        let indexes = Arc::new(Blobs::default());
        let scratch = tempfile::tempdir().unwrap();
        let sink = crate::sink::new(
            range.manifest().contract.clone(),
            recovery.clone(),
            indexes.clone(),
            server.ingestion.clone(),
            server.client.clone(),
            scratch.path(),
        )
        .unwrap();
        Self { scratch, recovery, range, sink, indexes }
    }
    async fn dispatch(&self) {
        dispatch_range(&self.sink, &self.range, &DeliveryAttempt::new(1).unwrap()).await.unwrap();
    }
    async fn publish(&self) -> culprit_collector::delivery::sink::Publication {
        let verified = verify(&self.sink, &self.range, VerificationBudget::default()).await.unwrap();
        let publication = self.sink.publish(&verified, &DeliveryAttempt::new(2).unwrap()).await.unwrap();
        assert!(self.sink.publication_visible(&publication, &self.range).await.unwrap());
        publication
    }
    async fn complete(&self) {
        self.dispatch().await;
        self.publish().await;
    }
}

#[tokio::test]
#[ignore = "requires the owned ClickHouse container from dev/smoke-clickhouse.py"]
async fn functional_contract_and_canonical_sets() {
    let server = Server::new();
    server.deploy();
    let catalog = SchemaCatalog::new().unwrap();
    native_scalar_roundtrip(&server).await;
    exact_delivery_and_recovery(&server, &catalog).await;
    complete_current_sets(&server, &catalog).await;
    native_query_semantics(&server, &catalog).await;
    unexpected_members_and_missing_rows(&server, &catalog).await;
    shared_resource_scopes(&server, &catalog).await;
    pool_history(&server, &catalog).await;
    observed_wait_resources(&server, &catalog).await;
    native_compilation_memory(&server, &catalog).await;
}

async fn pool_history(server: &Server, catalog: &SchemaCatalog) {
    let (source, record) = test_support::pool_history();
    let observation = source.observation.clone();
    let scenario = Scenario::new(
        server,
        Recovery::capture(server.client.destination(), catalog.digest(), source, vec![record], 1_500),
    )
    .await;
    scenario.complete().await;
    let rows = server
        .values(
            format!("SELECT ReplicaId, Metrics FROM {}", server.view("CulpritResourceSamplesV1Rows", false)),
            server.scope(&scenario.range),
            &[("ReplicaId", "Nullable(String)"), ("Metrics", "String")],
        )
        .await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0][0].is_null());
    let metrics: Value = serde_json::from_str(rows[0][1].as_str().unwrap()).unwrap();
    assert_eq!(metrics[0]["value"].as_f64(), Some(0.0));
    assert!(metrics[1]["value"].is_null());
    assert_eq!(metrics[2]["unit"], "bytes");
    assert_eq!(metrics[2]["value"].as_f64(), Some(536870912.0));
    assert_eq!(metrics[3]["describes_subject"], false);
    let coverage = server
        .values(
            "SELECT SourceDescription FROM CulpritPublishedRangesV1 WHERE RangeId={range:String}".into(),
            BTreeMap::from([("range".into(), scenario.range.manifest().range_id.clone())]),
            &[("SourceDescription", "String")],
        )
        .await;
    let description: Value = serde_json::from_str(coverage[0][0].as_str().unwrap()).unwrap();
    assert_eq!(description["observation"], serde_json::to_value(observation).unwrap());
    assert!(description["observation"]["data"]["nominal_period_ms"].is_null());
}

async fn native_compilation_memory(server: &Server, catalog: &SchemaCatalog) {
    let scenario =
        Scenario::new(server, test_support::query_compilation(server.client.destination(), catalog.digest())).await;
    scenario.complete().await;
    let text = format!(
        "SELECT JSONExtractString(ifNull(Details,'{{}}'),'query_id') AS QueryId,JSONExtractString(ifNull(Details,'{{}}'),'compilations') AS Compiles,JSONExtractFloat(ifNull(Details,'{{}}'),'memory','average_kb') AS AverageKb,JSONExtractFloat(ifNull(Details,'{{}}'),'memory','last_kb') AS LastKb,JSONExtractFloat(ifNull(Details,'{{}}'),'memory','maximum_kb') AS MaximumKb FROM {}",
        server.view("CulpritOperationalV1Current", true)
    );
    let mut parameters = server.scope(&scenario.range);
    parameters.remove("from");
    parameters.remove("until");
    parameters.insert("asof".into(), "2020-01-01 12:05:00".into());
    parameters.insert("age".into(), "3600000".into());
    let rows = server
        .values(
            text,
            parameters,
            &[
                ("QueryId", "String"),
                ("Compiles", "String"),
                ("AverageKb", "Float64"),
                ("LastKb", "Float64"),
                ("MaximumKb", "Float64"),
            ],
        )
        .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], json!("42"));
    assert_eq!(rows[0][1], json!("9007199254740993"));
    // ClickHouse can encode whole-valued Float64 cells without a decimal point in JSON.
    assert_eq!(
        rows[0][2..].iter().map(Value::as_f64).collect::<Vec<_>>(),
        vec![Some(32768.5), Some(16384.0), Some(65536.0)]
    );
}

async fn shared_resource_scopes(server: &Server, catalog: &SchemaCatalog) {
    let mut sequence = 1_000;
    for kind in
        [SharedResourceKind::Instance, SharedResourceKind::Pool, SharedResourceKind::Cluster, SharedResourceKind::Node]
    {
        for namespace in ["native-namespace-a", "native-namespace-b"] {
            let (source, record) = test_support::shared_resource(kind, namespace);
            let scenario = Scenario::new(
                server,
                Recovery::capture(server.client.destination(), catalog.digest(), source, vec![record], sequence),
            )
            .await;
            sequence += 1;
            scenario.complete().await;
            assert_eq!(server.count("CulpritResourceSamplesV1Rows", &scenario.range, false).await, 1);
            let rows = server.values(format!("SELECT ResourceKind, toBool(JSONExtractBool(arrayElement(JSONExtractArrayRaw(Metrics),1),'describes_subject')) AS OwnsSubject FROM {}", server.view("CulpritResourceSamplesV1Rows", false)), server.scope(&scenario.range), &[("ResourceKind", "String"), ("OwnsSubject", "Bool")]).await;
            assert_eq!(rows, vec![vec![serde_json::to_value(kind).unwrap(), json!(true)]]);
            let published = server
                .values(
                    "SELECT SourceDescription FROM CulpritPublishedRangesV1 WHERE RangeId={range:String}".into(),
                    BTreeMap::from([("range".into(), scenario.range.manifest().range_id.clone())]),
                    &[("SourceDescription", "String")],
                )
                .await;
            assert_eq!(published.len(), 1);
            let coverage: Value = serde_json::from_str(published[0][0].as_str().unwrap()).unwrap();
            assert!(coverage.get("target").is_none());
            assert_eq!(
                coverage["observation"],
                serde_json::to_value(&scenario.range.manifest().source.observation).unwrap()
            );
        }
    }
}

async fn native_scalar_roundtrip(server: &Server) {
    use culprit_collector::analytical::{
        RowProjection,
        projection::{ScalarType, digest},
        schema::{Field, LogicalSchema},
    };
    let schema = crate::schema::Schema::new(
        LogicalSchema::new(
            "ScalarFixture",
            [
                Field::required("Count", "/count", ScalarType::ExactCounter),
                Field::optional("Label", "/label", ScalarType::String),
                Field::required("At", "/at", ScalarType::Timestamp),
                Field::required("Facts", "/facts", ScalarType::JsonContainer),
                Field::required("Real", "/real", ScalarType::Real),
                Field::required("Signed", "/signed", ScalarType::Long),
                Field::required("Flag", "/flag", ScalarType::Bool),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    server.execute("CREATE TABLE IF NOT EXISTS ScalarFixture (Count Decimal(38,0),Label Nullable(String),At DateTime64(9,'UTC'),Facts String,Real Float64,Signed Int64,Flag Bool) ENGINE=MergeTree ORDER BY Count");
    server.execute("TRUNCATE TABLE ScalarFixture");
    let inputs = [
        json!({"count":"18446744073709551615","label":null,"at":"2020-01-01T00:00:00.000000001Z","facts":{"n":9007199254740993u64},"real":0.10000000000000002,"signed":i64::MIN,"flag":true}),
        json!({"count":"9007199254740993","label":"","at":"1900-01-01T00:00:00.000000002Z","facts":{"n":1},"real":1.0000000000000002,"signed":i64::MAX,"flag":false}),
    ];
    for input in &inputs {
        let values = schema.project(input).unwrap();
        let bytes = schema.encode(&values).unwrap();
        server.ingestion.insert("ScalarFixture", crate::client::InsertData::Control(bytes)).await.unwrap();
        let parameters = BTreeMap::from([("n".into(), input["count"].as_str().unwrap().into())]);
        let rows = server
            .client
            .query(QueryRequest {
                text: format!(
                    "SELECT {} FROM ScalarFixture AS Stored WHERE Stored.Count={{n:Decimal(38,0)}}",
                    schema.verification_projection()
                ),
                parameters,
                columns: schema.verification_columns(),
                row_limit: 1,
                byte_limit: crate::response::MAX_RESPONSE_BYTES,
            })
            .await
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            schema.returned_digest(&rows[0]).unwrap(),
            digest(&values).unwrap(),
            "native scalars retain exact counters, null/empty and each nanosecond"
        );
    }
}
async fn exact_delivery_and_recovery(server: &Server, catalog: &SchemaCatalog) {
    let first = Scenario::new(server, Recovery::new(server.client.destination(), catalog.digest())).await;
    first.dispatch().await;
    assert_eq!(server.count("CulpritResourceSamplesV1Rows", &first.range, false).await, 0);
    first.publish().await;
    assert_eq!(server.count("CulpritResourceSamplesV1Rows", &first.range, false).await, 1);
    dispatch_range(&first.sink, &first.range, &DeliveryAttempt::new(3).unwrap()).await.unwrap();
    verify(&first.sink, &first.range, VerificationBudget::default()).await.unwrap();
    assert_eq!(
        server.count("CulpritResourceSamplesV1Rows", &first.range, false).await,
        1,
        "physical retries do not multiply totals"
    );
    let Scenario { sink, recovery, range, indexes, scratch } = first;
    drop(sink);
    *recovery.fault.lock().unwrap() = Fault::Stall;
    let resumed = crate::sink::new(
        range.manifest().contract.clone(),
        recovery.clone(),
        indexes,
        server.ingestion.clone(),
        server.client.clone(),
        scratch.path(),
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), verify(&resumed, &range, VerificationBudget::default()))
        .await
        .unwrap()
        .unwrap();
    // Persisted digest indexes permit restart without downloading any native evidence again.
    let (manifest, mut row) = test_support::fixture();
    let TelemetryRecord::ResourceSample(sample) = &mut row.record else { unreachable!() };
    sample.metrics[0].value = Some(30.0);
    let later = Scenario::new(
        server,
        Recovery::capture(server.client.destination(), catalog.digest(), manifest.source, vec![row.record], 3),
    )
    .await;
    later.dispatch().await;
    let text = format!(
        "SELECT JSONExtractFloat(arrayElement(JSONExtractArrayRaw(Metrics),1),'value') AS Cpu FROM {}",
        server.view("CulpritResourceSamplesV1Rows", false)
    );
    assert_eq!(server.values(text.clone(), server.scope(&range), &[("Cpu", "Float64")]).await, vec![vec![json!(12.5)]]);
    let publication = later.publish().await;
    assert_eq!(server.values(text.clone(), server.scope(&range), &[("Cpu", "Float64")]).await, vec![vec![json!(30)]]);
    server.execute(&format!("INSERT INTO CulpritResourceSamplesV1 SELECT * REPLACE ('[{{\"canary\":true}}]' AS Metrics) FROM CulpritResourceSamplesV1 WHERE RangeId='{}' LIMIT 1",later.range.manifest().range_id));
    assert!(verify(&later.sink, &later.range, VerificationBudget::default()).await.is_err());
    assert_eq!(
        server.values(text, server.scope(&range), &[("Cpu", "Float64")]).await,
        vec![vec![json!(12.5)]],
        "a conflicting later revision cannot replace trusted values"
    );
    server.execute(&format!("INSERT INTO CulpritPublishedRangesV1 SELECT * REPLACE ('poisoned' AS ManifestSha256) FROM CulpritPublishedRangesV1 WHERE RangeId='{}' LIMIT 1",later.range.manifest().range_id));
    assert!(later.sink.publication_visible(&publication, &later.range).await.is_err());
}
async fn complete_current_sets(server: &Server, catalog: &SchemaCatalog) {
    let destination = server.client.destination();
    let [(source, records), partial, (mut empty_source, empty_records), unavailable]: [(
        CollectionDescription,
        Vec<TelemetryRecord>,
    ); 4] = test_support::activity_evidence_cases().try_into().unwrap();
    let expected_active = records.len() as u64;
    let active = Scenario::new(server, Recovery::capture(destination, catalog.digest(), source, records, 4)).await;
    active.complete().await;
    assert_eq!(server.count("CulpritActivityV1Current", &active.range, true).await, expected_active);
    for (sequence, (source, records)) in [(401, partial), (402, unavailable)] {
        let incomplete =
            Scenario::new(server, Recovery::capture(destination, catalog.digest(), source, records, sequence)).await;
        incomplete.complete().await;
        assert_eq!(
            server.count("CulpritActivityV1Current", &active.range, true).await,
            expected_active,
            "partial or unavailable activity cannot clear the last qualified complete set"
        );
    }
    let SourceObservation::Activity(snapshot) = &mut empty_source.observation else { unreachable!() };
    snapshot.epoch = Some("restarted-native-epoch".into());
    let empty =
        Scenario::new(server, Recovery::capture(destination, catalog.digest(), empty_source, empty_records, 403)).await;
    empty.dispatch().await;
    assert_eq!(server.count("CulpritActivityV1Current", &active.range, true).await, expected_active);
    empty.publish().await;
    assert_eq!(
        server.count("CulpritActivityV1Current", &active.range, true).await,
        0,
        "a published successful-empty head closes the prior activity set across native epochs"
    );
    let counters = Scenario::new(server, test_support::waits(destination, catalog.digest(), 6, false, true)).await;
    counters.complete().await;
    assert_eq!(server.count("CulpritWaitCountersV1Current", &counters.range, true).await, 1);
    let partial = Scenario::new(server, test_support::waits(destination, catalog.digest(), 7, true, false)).await;
    partial.complete().await;
    assert_eq!(
        server.count("CulpritWaitCountersV1Current", &counters.range, true).await,
        1,
        "partial visibility never replaces a complete head"
    );
    let empty = Scenario::new(server, test_support::waits(destination, catalog.digest(), 8, true, true)).await;
    empty.complete().await;
    assert_eq!(server.count("CulpritWaitCountersV1Current", &counters.range, true).await, 0);
}
async fn native_query_semantics(server: &Server, catalog: &SchemaCatalog) {
    let history =
        Scenario::new(server, test_support::interval_queries(server.client.destination(), catalog.digest())).await;
    history.complete().await;
    let query = format!(
        "SELECT toString(Executions) AS N,JSONExtractUInt(arrayElement(JSONExtractArrayRaw(Metrics),2),'unit','bytes_per_page') AS PageBytes,isNull(PlanDefinitionId) AS MissingPlan FROM {}",
        server.view("CulpritQueryRuntimeV1Rows", false)
    );
    assert_eq!(
        server
            .values(
                query,
                server.scope(&history.range),
                &[("N", "String"), ("PageBytes", "UInt64"), ("MissingPlan", "UInt8")]
            )
            .await,
        vec![vec![json!("9007199254740993"), json!(16384), json!(1)]]
    );
    let cumulative =
        Scenario::new(server, test_support::cumulative_queries(server.client.destination(), catalog.digest())).await;
    cumulative.complete().await;
    assert_eq!(server.count("CulpritOperationalV1Current", &cumulative.range, true).await, 1);
    let mut scope = server.scope(&cumulative.range);
    scope.remove("from");
    scope.remove("until");
    scope.insert("asof".into(), "2020-01-01 12:05:00".into());
    scope.insert("age".into(), "3600000".into());
    let query = format!(
        "SELECT JSONExtractString(arrayElement(JSONExtractArrayRaw(ifNull(Details,'{{}}'),'totals'),1),'total','value') AS N,JSONExtractString(ifNull(Details,'{{}}'),'reset','kind') AS Reset FROM {}",
        server.view("CulpritOperationalV1Current", true)
    );
    assert_eq!(
        server.values(query, scope, &[("N", "String"), ("Reset", "String")]).await,
        vec![vec![json!("9007199254740993"), json!("unknown")]]
    );
}
async fn unexpected_members_and_missing_rows(server: &Server, catalog: &SchemaCatalog) {
    for (offset, mutation) in ["RowOrdinal", "BatchId", "table", "missing"].into_iter().enumerate() {
        let (mut manifest, row) = test_support::fixture();
        manifest.source.source = format!("fault-{mutation}");
        let scenario = Scenario::new(
            server,
            Recovery::capture(
                server.client.destination(),
                catalog.digest(),
                manifest.source,
                vec![row.record],
                100 + offset as u64,
            ),
        )
        .await;
        scenario.complete().await;
        let range = &scenario.range.manifest().range_id;
        let sql = match mutation {
            "RowOrdinal" => format!(
                "INSERT INTO CulpritResourceSamplesV1 SELECT * REPLACE (99 AS RowOrdinal) FROM CulpritResourceSamplesV1 WHERE RangeId='{range}' LIMIT 1"
            ),
            "BatchId" => format!(
                "INSERT INTO CulpritResourceSamplesV1 SELECT * REPLACE ('unexpected' AS BatchId) FROM CulpritResourceSamplesV1 WHERE RangeId='{range}' LIMIT 1"
            ),
            "table" => format!(
                "INSERT INTO CulpritWaitCountersV1 SELECT * REPLACE ('{range}' AS RangeId) FROM CulpritWaitCountersV1 LIMIT 1"
            ),
            "missing" => {
                format!("ALTER TABLE CulpritResourceSamplesV1 DELETE WHERE RangeId='{range}' SETTINGS mutations_sync=2")
            }
            _ => unreachable!(),
        };
        server.execute(&sql);
        assert!(
            verify(&scenario.sink, &scenario.range, VerificationBudget::default()).await.is_err(),
            "{mutation} must invalidate delivery proof"
        );
    }
}

async fn observed_wait_resources(server: &Server, catalog: &SchemaCatalog) {
    use culprit_core::WaitResourceKind;
    for (offset, kind) in
        [WaitResourceKind::MemoryPage, WaitResourceKind::PageIo, WaitResourceKind::DataLock].into_iter().enumerate()
    {
        let scenario = Scenario::new(
            server,
            test_support::activity_with_wait_resource(
                server.client.destination(),
                catalog.digest(),
                2_000 + offset as u64,
                kind,
            ),
        )
        .await;
        scenario.complete().await;
        let text = format!(
            "SELECT JSONExtractString(arrayElement(JSONExtractArrayRaw(ifNull(WaitResources,'[]')),1),'kind') AS Kind,JSONExtractString(arrayElement(JSONExtractArrayRaw(ifNull(WaitResources,'[]')),1),'native','id') AS NativeId FROM {}",
            server.view("CulpritActivityV1Current", true)
        );
        let mut parameters = server.scope(&scenario.range);
        parameters.remove("from");
        parameters.remove("until");
        parameters.insert("asof".into(), "2020-01-01 12:05:00".into());
        parameters.insert("age".into(), "3600000".into());
        assert_eq!(
            server.values(text, parameters, &[("Kind", "String"), ("NativeId", "String")]).await,
            vec![vec![serde_json::to_value(kind).unwrap(), json!("native-wait-resource")]]
        );
    }
}
