//! One read-only workload on each explicitly owned AG member, through the real provider and collector.
use crate::{
    MssqlProvider,
    conn::{MsClient, SessionKind},
    read::{integer, text},
    test_support::{
        OwnedSourceFixture, batch, config, connect,
        owned_captures::{Captured, OwnedCaptures},
        receipt_directory, save_cost_receipt,
    },
    value::Rec,
};
use culprit_collector::capture::{CollectionCancellation, SourceFactory};
use culprit_core::{Engine, Environment, Provider, Result, telemetry::*};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    panic::{AssertUnwindSafe, resume_unwind},
    time::Duration,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

struct Factory(MssqlProvider);
#[async_trait::async_trait]
impl SourceFactory for Factory {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        let recipe = crate::telemetry::recipes().into_iter().find(|recipe| recipe.selector.matches(request)).unwrap();
        assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
        let source = self.0.telemetry_source(request)?;
        assert_eq!(source.query_count(), 0, "the actual factory does not acquire before the collector owns it");
        Ok(source)
    }
}

fn sql(database: &str, secondary_only: bool) -> String {
    let database = Engine::Mssql.dialect().quote(database);
    if secondary_only {
        format!("SELECT MAX(DATALENGTH(owner)) AS readable_secondary_only FROM {database}.dbo.culprit_ag_marker")
    } else {
        format!("SELECT COUNT_BIG(*) AS readable_member_common FROM {database}.dbo.culprit_ag_marker")
    }
}

async fn scalar(client: &mut MsClient, sql: &str) -> i64 {
    let rows = tokio::time::timeout(COMMAND_TIMEOUT, async {
        client.simple_query(sql).await.unwrap().into_results().await.unwrap()
    })
    .await
    .expect("owned SELECT must finish within its fixture deadline");
    let [rows] = rows.as_slice() else { panic!("one owned result set") };
    let [row] = rows.as_slice() else { panic!("one owned scalar") };
    assert_eq!(row.columns().len(), 1);
    let row = Rec::from_row(row);
    let (column, _) = row.columns().next().unwrap();
    integer(&row, column).expect("native integral scalar")
}

async fn session(client: &mut MsClient) -> NativeActivityIdentity {
    let row = tokio::time::timeout(COMMAND_TIMEOUT, crate::conn::query_one(client,
        "SELECT /* culprit fixture */ CONVERT(bigint,@@SPID) AS id,CONVERT(varchar(40),CAST(login_time AS datetime2),126) AS generation FROM sys.dm_exec_sessions WHERE session_id=@@SPID"
    )).await.unwrap().unwrap();
    NativeActivityIdentity {
        id: integer(&row, "id").unwrap().to_string(),
        generation: crate::sql_time::native_time(&text(&row, "generation").unwrap()).unwrap(),
    }
}

async fn database_id(client: &mut MsClient) -> u32 {
    let row = tokio::time::timeout(
        COMMAND_TIMEOUT,
        crate::conn::query_one(client, "SELECT /* culprit fixture */ CONVERT(bigint,DB_ID()) AS id"),
    )
    .await
    .unwrap()
    .unwrap();
    u32::try_from(integer(&row, "id").unwrap()).unwrap()
}

async fn cache_contexts(client: &mut MsClient, sql: &str) -> Vec<CachedStatementContext> {
    let query_sql = format!(
        "SELECT TOP(4) /* culprit fixture */ {context} {relation} WHERE pa.attribute='dbid' AND st.text=@P1 OPTION(MAXDOP 1)",
        context = crate::cached_statements::context_columns(),
        relation = crate::cached_statements::relation(),
    );
    let rows = tokio::time::timeout(COMMAND_TIMEOUT, async {
        let mut query = tiberius::Query::new(query_sql);
        query.bind(sql);
        query.query(client).await.unwrap().into_results().await.unwrap()
    })
    .await
    .expect("bounded independent native cache oracle");
    let [rows] = rows.as_slice() else { panic!("one bounded native cache relation") };
    assert!(rows.len() <= 3, "the fourth row witnesses unexpected cache ambiguity");
    rows.iter().map(|row| crate::cached_statements::context(&Rec::from_row(row)).unwrap()).collect()
}

fn native_context(record: &TelemetryRecord) -> &CachedStatementContext {
    match record {
        TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::MssqlCachedRuntime(row), ..
        }) => &row.context,
        TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {
            fact: ObjectDefinitionFact::MssqlCachedDefinition(row),
            ..
        }) => &row.context,
        TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {
            fact: ObjectDefinitionFact::MssqlRetainedPlan(row),
            ..
        }) => &row.context,
        _ => panic!("the selected original cache recipe must retain its declared native family"),
    }
}

fn assert_original_cache(
    capture: &Captured,
    activity: &Captured,
    expected: &[CachedStatementContext],
    foreign: &CachedStatementContext,
) -> Value {
    let source = capture.source().unwrap();
    assert!(source.partition.is_qualified());
    assert_eq!(source.partition, activity.source().unwrap().partition);
    assert_eq!(source.target, activity.source().unwrap().target);
    assert!(
        source.target.database().unwrap().incarnation.is_none(),
        "native local GUIDs never become a global database identity"
    );
    let snapshot = source.observation.snapshot().unwrap();
    assert!(matches!(
        snapshot.dataset,
        SnapshotDataset::CachedRuntime | SnapshotDataset::CachedDefinitions | SnapshotDataset::RetainedPlans
    ));
    assert!(!capture.records.is_empty());
    assert!(capture.records.iter().all(|row| native_context(&row.record).database_id == expected[0].database_id));
    assert!(
        !capture.records.iter().any(|row| native_context(&row.record) == foreign),
        "same SQL compiled in master must remain outside the selected original database"
    );
    let mut selected = Vec::new();
    for context in expected {
        let rows = capture.records.iter().filter(|row| native_context(&row.record) == context).collect::<Vec<_>>();
        let [row] = rows.as_slice() else { panic!("one exact original native statement incarnation per owned SELECT") };
        match &row.record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::MssqlCachedRuntime(runtime),
                ..
            }) => {
                assert_eq!(runtime.executions.as_deref(), Some("1"));
                assert_eq!(runtime.reset, culprit_core::ResetMarker::Unknown);
                assert!(runtime.measured());
            }
            TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {
                fact: ObjectDefinitionFact::MssqlCachedDefinition(definition),
                ..
            }) => {
                assert!(definition.statement.payload.is_available());
                assert!(
                    definition.estimated_plan.payload.is_available(),
                    "a genuine native estimated plan must be retained"
                );
            }
            TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {
                fact: ObjectDefinitionFact::MssqlRetainedPlan(definition),
                ..
            }) => {
                assert!(definition.statement.payload.is_available());
                // Keep the native optional setting and payload as observed. A setting alone does not exclude
                // another native capture route, or make an absent retained body an actual measurement.
                assert!(definition.is_valid());
            }
            _ => unreachable!(),
        }
        selected.push(json!({"context":context,"record_id":row.record_id}));
    }
    json!({"source":source,"capture":capture.manifest.capture,"source_queries":capture.manifest.source_queries,
        "selected":selected,"native_work_settled":!capture.manifest.read_metrics.as_ref().unwrap().has_unconfirmed_work(),
        "status":capture.manifest.status,"production_cost_qualified":false})
}

#[tokio::test]
#[ignore = "requires the explicit nonce-owned readable CU27 AG wrapper; no production endpoints"]
async fn owned_readable_ag_workload_uses_real_factory_and_isolates_original_member_cache_populations() {
    let role = std::env::var("CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE").expect("explicit owned AG role");
    assert!(matches!(role.as_str(), "primary" | "secondary"));
    let secondary = role == "secondary";
    let fixture = tokio::time::timeout(COMMAND_TIMEOUT, OwnedSourceFixture::open("culprit-readable-workload-source"))
        .await
        .unwrap()
        .unwrap();
    let mut actor = connect(
        &config(fixture.port, &fixture.database, fixture.password(), "culprit-owned-readable-workload"),
        SessionKind::User,
    )
    .await
    .unwrap();
    let mut master =
        connect(&config(fixture.port, "master", fixture.password(), "culprit-owned-foreign-cache"), SessionKind::User)
            .await
            .unwrap();
    let result = AssertUnwindSafe(async {
        let selected_database_id = database_id(&mut actor).await;
        assert!(selected_database_id > 4);
        assert_eq!(database_id(&mut master).await, 1);
        let actor_id = session(&mut actor).await;
        let common = sql(&fixture.database, false);
        let secondary_sql = sql(&fixture.database, true);
        tokio::time::timeout(COMMAND_TIMEOUT, batch(&mut actor, "BEGIN TRANSACTION")).await.unwrap();
        assert_eq!(scalar(&mut actor, &common).await, 1);
        if secondary {
            assert_eq!(scalar(&mut actor, &secondary_sql).await, 24);
        }
        assert_eq!(scalar(&mut master, &common).await, 1, "same SQL is also cached under a foreign compiling database");
        let oracle = cache_contexts(&mut master, &common).await;
        let originals = oracle.iter().filter(|row| row.database_id == selected_database_id).collect::<Vec<_>>();
        let [original] = originals.as_slice() else { panic!("one local native compile context") };
        let original = (**original).clone();
        let foreigners = oracle.iter().filter(|row| row.database_id == 1).collect::<Vec<_>>();
        let [foreign] = foreigners.as_slice() else { panic!("one independent master native compile context") };
        let foreign = (**foreign).clone();
        assert_ne!(original.identity, foreign.identity);
        let mut expected = vec![original];
        if secondary {
            let rows = cache_contexts(&mut actor, &secondary_sql).await;
            let [only] = rows.as_slice() else { panic!("one secondary-only native statement") };
            assert_eq!(only.database_id, selected_database_id);
            expected.push(only.clone());
        }
        let factory = Factory(MssqlProvider { endpoint_host:"127.0.0.1".into(), endpoint_port:fixture.port,
            environment:Environment::Development, pool:fixture.pool.clone() });
        let mut captures = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port).unwrap();
        let cancellation = CollectionCancellation::default();
        let activity = captures.capture_with_factory(CollectionSelection::Activity, &cancellation, &factory).await.unwrap();
        let native = activity.source().unwrap().observation.activity().unwrap();
        assert_eq!(native.database_writable, Some(!secondary), "the native source, rather than ApplicationIntent or the test label, supplies updateability");
        activity.with_activity_frame(|frame| {
            assert!(frame.is_qualified());
            assert!(frame.session_rows(&actor_id).any(|row| row.observation.idle && row.observation.open_transactions.is_some_and(|count| count > 0)));
            Ok(())
        }).unwrap();
        let mut receipts = Vec::new();
        for dataset in [SnapshotDataset::CachedRuntime, SnapshotDataset::CachedDefinitions, SnapshotDataset::RetainedPlans] {
            let captured = captures.capture_with_factory(CollectionSelection::Snapshot { dataset }, &cancellation, &factory).await.unwrap();
            receipts.push(assert_original_cache(&captured, &activity, &expected, &foreign));
            captures.verify_retained(&captured).await.unwrap();
        }
        captures.verify_retained(&activity).await.unwrap();
        let receipt = json!({"case":"readable-member-local-workload","expected_role":role,"native_activity_source":activity.source().unwrap(),
            "native_actor":actor_id,"source_recipes":receipts,"shared_query_store_read":false,
            "cache_walk_server_work_bound":"unknown; TOP and payload limits do not bound the server-wide search",
            "production_cost_qualified":false,"native_role_coverage":"this one observed member only"});
        save_cost_receipt(&receipt_directory(), "readable-workload", &receipt).unwrap();
    }).catch_unwind().await;
    let rollback = if result.is_ok() {
        Some(
            tokio::time::timeout(COMMAND_TIMEOUT, async {
                actor.simple_query("IF @@TRANCOUNT>0 ROLLBACK TRANSACTION").await?.into_results().await
            })
            .await,
        )
    } else {
        // A failed or timed-out response may still own the socket. Retire it instead of issuing follow-up SQL.
        None
    };
    drop(actor);
    drop(master);
    if let Err(error) = result {
        resume_unwind(error);
    }
    assert!(
        matches!(rollback, Some(Ok(Ok(_)))),
        "the original read-only workload transaction must drain or its actor be retired"
    );
}
