//! Disposable loopback SQL Server fixture helpers; production endpoints are never accepted.
use crate::conn::MsClient;
use std::time::Duration;
use tiberius::{AuthMethod, Config, EncryptionLevel};

pub(crate) fn config(port: u16, database: &str, password: &str, application: &str) -> Config {
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.port(port);
    config.database(database);
    config.application_name(application);
    config.authentication(AuthMethod::sql_server("sa", password));
    config.encryption(EncryptionLevel::Off);
    config.trust_cert();
    config.command_timeout(Some(Duration::from_secs(40)));
    config
}
pub(crate) async fn batch(client: &mut MsClient, sql: &str) {
    client.simple_query(sql).await.unwrap().into_results().await.unwrap();
}

pub(crate) async fn session_id(client: &mut MsClient) -> String {
    crate::conn::query_one(client, "SELECT /* culprit fixture */ CONVERT(varchar(16),@@SPID) AS id")
        .await
        .unwrap()
        .string("id")
}

/// An independent fixture session. It never shares the production reader's uncertainty fence.
pub(crate) async fn connect(config: &Config, kind: crate::conn::SessionKind) -> culprit_core::Result<MsClient> {
    crate::conn::Pool::new(config.clone(), None).connect(kind).await
}

pub(crate) async fn query_store_ready(client: &mut crate::conn::MsClient) {
    const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
    const READINESS_POLL: Duration = Duration::from_millis(50);
    const WARMUP: &str = "SELECT COUNT_BIG(*) AS ready FROM dbo.query_store_readiness_fixture";
    batch(client, "CREATE TABLE dbo.query_store_readiness_fixture(id int NOT NULL);INSERT dbo.query_store_readiness_fixture VALUES(1)").await;
    let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
    loop {
        let options =
            crate::conn::query_one(client, "SELECT actual_state,readonly_reason FROM sys.database_query_store_options")
                .await
                .unwrap();
        if options.f64("actual_state") == Some(2.0) {
            batch(client, WARMUP).await;
            batch(client, "EXEC sys.sp_query_store_flush_db").await;
            let captured = crate::conn::query_one(client, &format!(
                "SELECT COUNT_BIG(*) AS captured FROM sys.query_store_query_text qt JOIN sys.query_store_query q ON q.query_text_id=qt.query_text_id JOIN sys.query_store_plan p ON p.query_id=q.query_id JOIN sys.query_store_runtime_stats rs ON rs.plan_id=p.plan_id WHERE qt.query_sql_text=N'{}' AND rs.count_executions>0", WARMUP.replace('\'', "''")
            )).await.unwrap();
            if captured.f64("captured").is_some_and(|count| count > 0.0) {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Query Store did not acknowledge fixture capture readiness: {options:?}"
        );
        tokio::time::sleep(READINESS_POLL).await;
    }
}

pub(crate) async fn assert_no_execution_grant(
    client: &mut MsClient,
    sql: &str,
    parameters: Vec<crate::read::Parameter>,
) -> Vec<String> {
    // Actual plan instrumentation is confined to the owned fixture and runs only Culprit's metadata statements.
    batch(client, "SET STATISTICS XML ON").await;
    let mut query = tiberius::Query::new(sql);
    for parameter in &parameters {
        match parameter {
            crate::read::Parameter::Integer(value) => {
                query.bind(*value);
            }
            crate::read::Parameter::Text(value) => {
                query.bind(value.as_str());
            }
        }
    }
    let results = query.query(&mut *client).await.unwrap().into_results().await.unwrap();
    batch(client, "SET STATISTICS XML OFF").await;
    let mut plans = Vec::new();
    for rows in results {
        for row in rows {
            let row = crate::value::Rec::from_row(&row);
            for (_, value) in row.columns() {
                let crate::value::Val::Text(value) = value else { continue };
                if !value.contains("<ShowPlanXML") {
                    continue;
                }
                culprit_core::xml::read(value, |document| {
                    let document = document.unwrap();
                    assert_eq!(document.root_element().tag_name().name(), "ShowPlanXML");
                    for node in document.descendants().filter(|node| node.is_element()) {
                        assert!(
                            !matches!(node.attribute("PhysicalOp"), Some("Hash Match" | "Sort")),
                            "grant-bearing operator in metadata diagnostic: {value}"
                        );
                        if node.tag_name().name() == "MemoryGrantInfo" {
                            for name in [
                                "GrantedMemory",
                                "RequestedMemory",
                                "RequiredMemory",
                                "DesiredMemory",
                                "SerialRequiredMemory",
                                "SerialDesiredMemory",
                                "MaxUsedMemory",
                            ] {
                                assert!(
                                    node.attribute(name).is_none_or(|value| value.parse::<u64>() == Ok(0)),
                                    "{name} in diagnostic actual plan: {value}"
                                );
                            }
                        }
                    }
                });
                plans.push(value.clone());
            }
        }
    }
    assert!(!plans.is_empty(), "actual metadata plan was not returned");
    plans
}
