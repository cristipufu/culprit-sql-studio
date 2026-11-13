//! Only this owned fixture executes application SQL or changes Query Store options.
use super::*;
use crate::{
    conn::{Pool, SessionKind},
    query_store::{Reader, catalog_sql, decode, definitions},
    read::{NativeRead, Parameter},
    test_support::{batch, config, session_id},
};
use culprit_core::{
    SnapshotOptions,
    telemetry::{SourceShutdown, StatementReferences, TelemetryRecord, TelemetryStream, WaitCaptureCoverage},
};
use futures_util::FutureExt;
use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

async fn qualify_cost_plan(
    client: &mut crate::conn::MsClient,
    window: &super::super::window_sql::FrozenWindow,
    ids: &[String],
) {
    let sql = super::super::costs::runs_sql(true);
    batch(client, "SET STATISTICS XML ON").await;
    let mut query = tiberius::Query::new(&sql);
    let Parameter::Text(scope) = window.parameter() else { panic!("native interval scope") };
    let Parameter::Text(queries) = super::super::reader::ids_parameter(ids).unwrap() else { panic!("query IDs") };
    query.bind(scope.as_str());
    query.bind(queries.as_str());
    let rows = query.query(&mut *client).await.unwrap().into_results().await.unwrap();
    batch(client, "SET STATISTICS XML OFF").await;
    let mut plans = 0;
    let mut selected_groups = 0;
    for result in rows {
        for row in result {
            let row = Rec::from_row(&row);
            for (_, value) in row.columns() {
                let crate::value::Val::Text(xml) = value else { continue };
                if !xml.contains("<ShowPlanXML") {
                    continue;
                }
                if let Ok(path) = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PLAN_PATH") {
                    std::fs::write(path, xml).unwrap();
                }
                plans += 1;
                culprit_core::xml::read(xml, |document| {
                    let document = document.unwrap();
                    let mut cpu_ms = None;
                    let mut granted_kib = None;
                    let mut logical_reads_per_operator_max = None;
                    for node in document.descendants().filter(|node| node.is_element()) {
                        assert!(
                            !matches!(node.attribute("PhysicalOp"), Some("Window Spool" | "Window Aggregate")),
                            "cost source must not materialize all-query window groups: {xml}"
                        );
                        if node.tag_name().name() == "MemoryGrantInfo"
                            && let Some(value) =
                                node.attribute("GrantedMemory").and_then(|value| value.parse::<u64>().ok())
                        {
                            granted_kib = Some(granted_kib.unwrap_or(0).max(value));
                        }
                        if node.tag_name().name() == "QueryTimeStats"
                            && let Some(value) = node.attribute("CpuTime").and_then(|value| value.parse::<u64>().ok())
                        {
                            cpu_ms = Some(cpu_ms.unwrap_or(0) + value);
                        }
                        if let Some(value) =
                            node.attribute("ActualLogicalReads").and_then(|value| value.parse::<u64>().ok())
                        {
                            logical_reads_per_operator_max =
                                Some(logical_reads_per_operator_max.unwrap_or(0).max(value));
                        }
                        if node.tag_name().name() == "RelOp" && node.attribute("LogicalOp") == Some("Aggregate") {
                            // Native views expose internal Union/Expr keys. Identify this operator's non-scalar
                            // grouping directly, rather than infer SQL aliases or inspect a descendant's aggregate.
                            let selected = node
                                .children()
                                .flat_map(|operator| operator.children())
                                .filter(|child| matches!(child.tag_name().name(), "GroupBy" | "HashKeysBuild"))
                                .any(|group| {
                                    group.descendants().any(|column| column.tag_name().name() == "ColumnReference")
                                });
                            if selected {
                                selected_groups += 1;
                                let actual = node
                                    .children()
                                    .find(|child| child.tag_name().name() == "RunTimeInformation")
                                    .unwrap();
                                let rows = actual
                                    .children()
                                    .filter_map(|thread| thread.attribute("ActualRows"))
                                    .map(|rows| rows.parse::<f64>().unwrap())
                                    .sum::<f64>();
                                assert!(
                                    rows <= ids.len() as f64,
                                    "selected aggregate produced {rows} groups for {} IDs",
                                    ids.len()
                                );
                            }
                        }
                    }
                    eprintln!(
                        "owned cost actual plan: selected IDs={}, granted workspace KiB={granted_kib:?}, CPU ms={cpu_ms:?}, maximum reported operator logical reads={logical_reads_per_operator_max:?}",
                        ids.len()
                    );
                });
            }
        }
    }
    assert!(plans > 0 && selected_groups > 0, "actual selected-group qualification was not returned");
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_query_store_wait_ids_semantics_and_scan_totals_agree_without_historical_absence_claims() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_waits_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    batch(&mut admin,&format!("CREATE DATABASE [{database}];ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=120;ALTER DATABASE [{database}] SET QUERY_STORE=ON(OPERATION_MODE=READ_WRITE,QUERY_CAPTURE_MODE=ALL,WAIT_STATS_CAPTURE_MODE=ON)")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let mut setup =
        crate::test_support::connect(&config(port, &database, &password, "wait-semantics-fixture"), SessionKind::User)
            .await
            .unwrap();
    crate::test_support::query_store_ready(&mut setup).await;
    batch(&mut setup,"CREATE TABLE dbo.wait_semantics_fixture(id int PRIMARY KEY,payload int NOT NULL);INSERT dbo.wait_semantics_fixture VALUES(1,1)").await;
    let mut owner =
        crate::test_support::connect(&config(port, &database, &password, "wait-semantics-owner"), SessionKind::User)
            .await
            .unwrap();
    let mut affected =
        crate::test_support::connect(&config(port, &database, &password, "wait-semantics-affected"), SessionKind::User)
            .await
            .unwrap();
    let affected_id = session_id(&mut affected).await;
    batch(&mut owner, "BEGIN TRANSACTION;UPDATE dbo.wait_semantics_fixture SET payload=2 WHERE id=1").await;
    let mut waiting = Some(tokio::spawn(async move {
        batch(&mut affected, "SELECT payload AS culprit_wait_native_proof FROM dbo.wait_semantics_fixture WHERE id=1")
            .await;
    }));
    let proof = AssertUnwindSafe(async {
        let witness_sql = format!("SELECT TOP(1) wait_type FROM sys.dm_exec_requests WHERE session_id={} OPTION(MAXDOP 1)",affected_id.parse::<u16>().unwrap());
        let started = Instant::now();
        loop {
            let rows = crate::conn::query(&mut setup,&witness_sql).await.unwrap();
            if rows.first().and_then(|row|row.str("wait_type")).is_some_and(|wait|wait.starts_with("LCK_M_")) {
                break;
            }
            assert!(started.elapsed()<Duration::from_secs(10),"owned affected request did not enter a native lock wait");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        batch(&mut owner,"COMMIT TRANSACTION").await;
        let completion = tokio::time::timeout(Duration::from_secs(10),waiting.as_mut().unwrap()).await;
        if completion.is_ok() { drop(waiting.take()); }
        completion.unwrap().unwrap();
        batch(&mut setup,"SELECT SUM(payload) AS culprit_unselected_native_proof FROM dbo.wait_semantics_fixture").await;
        batch(&mut setup,"EXEC sys.sp_query_store_flush_db").await;

        let mut reader = Reader::open(pool.clone());
        let summary = reader.summary(60).await.unwrap();
        let ids = summary.iter().map(|row|row.query_id.clone()).collect::<Vec<_>>();
        let definitions = reader.definitions(&ids,definitions::Kind::Query,256*1024).await.unwrap();
        let query = definitions.iter().find(|definition|definition.payload.text().is_some_and(|text|text.contains("culprit_wait_native_proof"))).unwrap();
        let other = definitions.iter().find(|definition|definition.payload.text().is_some_and(|text|text.contains("culprit_unselected_native_proof"))).unwrap();
        assert_ne!(query.id,other.id,"the fixture must contain an actual unselected workload query");
        let plans = summary.iter().filter(|row|row.query_id==query.id).map(|row|row.plan_id.clone()).collect::<Vec<_>>();
        let window = reader.window(60).await.unwrap();
        let metadata = semantics(window.capture);
        assert_eq!(metadata.capture,WaitCaptureState::Enabled);
        assert_eq!(metadata.coverage,WaitCaptureCoverage::Unverified);
        assert!(metadata.is_valid());
        qualify_cost_plan(&mut setup,&window,std::slice::from_ref(&query.id)).await;
        let observed = reader.waits(60,Some(&plans)).await.unwrap();
        let mut analytical_native_totals = BTreeMap::<u8,u64>::new();
        for row in &observed {
            let meaning = &metadata.categories[&row.category_id.to_string()];
            assert_eq!(meaning.native_name,row.category);
            *analytical_native_totals.entry(row.category_id).or_default() += row.total_ms;
        }
        let mut raw_totals = BTreeMap::<u8,u64>::new();
        for interval in window.interval_ids() {
            let rows = reader.read(&catalog_sql::waits(false),vec![Parameter::Integer(1000),Parameter::Integer(interval)],1000).await.unwrap();
            for row in rows.into_iter().filter(|row|plans.contains(&row.string("plan_id"))) {
                let TelemetryRecord::QueryWaits(raw) = decode::record(&row,TelemetryStream::QueryWaits,StatementReferences::default()).unwrap() else {panic!("native wait")};
                let id = raw.category.parse::<u8>().unwrap();
                assert_eq!(raw.category,id.to_string(),"raw native category IDs are retained");
                *raw_totals.entry(id).or_default() += raw.total.integer().unwrap();
            }
        }
        assert_eq!(analytical_native_totals,raw_totals);
        assert!(raw_totals.get(&3).is_some_and(|total|*total>0),"native lock waiting must reach Query Store");
        let folded = fold(&observed);
        assert!(folded.values.iter().any(|wait|wait.category==WaitClass::Lock.category()&&wait.wait_ms>0.0));
        assert_eq!(reader.verify_window().await.unwrap(), super::super::window_sql::WindowVerification::Unchanged);
        let scan = crate::workload::from_query_store(&mut reader,&SnapshotOptions{window_minutes:60,plans:0,..Default::default()}).await.unwrap();
        let scan_query = scan.workload.queries.iter().find(|row|row.id==query.id).unwrap();
        assert!(scan_query.observed_wait_ms(WaitClass::Lock).is_some_and(|total|total>0.0));
        assert!(scan_query.measured_wait_ms(WaitClass::Network).is_none(),"current ON does not qualify missing historical waits");
        assert!(!scan_query.waits_coverage.as_ref().unwrap().complete);
        assert!(scan_query.waits.iter().all(|wait|wait.wait_ms>0.0));
        let final_window = reader.window(60).await.unwrap();
        assert_eq!(scan.workload.window_start,final_window.start);
        assert_eq!(scan.workload.window_end,final_window.end);

        batch(&mut setup,&format!("ALTER DATABASE [{database}] SET QUERY_STORE(WAIT_STATS_CAPTURE_MODE=OFF)")).await;
        assert_eq!(reader.verify_window().await.unwrap(),super::super::window_sql::WindowVerification::WaitCaptureChanged,"mode changes cannot verify the frozen description");
        assert_eq!(reader.close().await,SourceShutdown::Quiescent);
        let mut disabled = Reader::open(pool.clone());
        let scan = crate::workload::from_query_store(&mut disabled,&SnapshotOptions{window_minutes:60,plans:0,..Default::default()}).await.unwrap();
        assert_eq!(disabled.window(60).await.unwrap().capture,WaitCaptureState::Disabled);
        let scan_query = scan.workload.queries.iter().find(|row|row.id==query.id).unwrap();
        assert!(scan_query.observed_wait_ms(WaitClass::Lock).is_some_and(|total|total>0.0),"disabling capture does not erase observed historical waits");
        assert!(scan_query.measured_wait_ms(WaitClass::Lock).is_none());
        assert!(!scan_query.waits_coverage.as_ref().unwrap().complete);
        let metrics = disabled.read_metrics().unwrap();
        eprintln!("owned Query Store waits: {} bounded source queries, {:.3} client milliseconds; native lock subtotal={} ms; historical capture unverified",
            disabled.queries(),metrics.templates.iter().map(|metric|metric.client_elapsed_ms).sum::<f64>(),raw_totals[&3]);
        assert_eq!(disabled.close().await,SourceShutdown::Quiescent);
    }).catch_unwind().await;
    batch(&mut owner, "IF @@TRANCOUNT>0 ROLLBACK TRANSACTION").await;
    if let Some(mut task) = waiting
        && tokio::time::timeout(Duration::from_secs(10), &mut task).await.is_err()
    {
        task.abort();
        let _ = task.await;
    }
    drop(owner);
    drop(setup);
    drop(pool);
    batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
    if let Err(failure) = proof {
        std::panic::resume_unwind(failure);
    }
}
