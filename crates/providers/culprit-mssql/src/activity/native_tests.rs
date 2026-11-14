//! Local-only integration: never accepts a remote hostname or touches an existing database.
use super::*;
use crate::conn::SessionKind;
use crate::test_support::session_id;
use crate::test_support::{batch, config};
use std::time::{Duration, Instant};

fn options(scope: ActivityScope) -> ReadOptions {
    ReadOptions { budget: CollectionBudget::default(), scope, identity: IdentityDetail::Endpoint }
}

#[tokio::test]
#[ignore = "requires a disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_blocking_uses_the_shared_reader_for_live_and_export_at_compatibility_120() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT")
        .expect("set the disposable loopback port")
        .parse::<u16>()
        .unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").expect("set the disposable database password");
    let database = format!("culprit_activity_{}_{}", std::process::id(), chrono::Utc::now().timestamp_millis());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "culprit"), SessionKind::User).await.unwrap();
    batch(&mut admin, &format!("CREATE DATABASE [{database}] COLLATE Latin1_General_100_CI_AS_SC")).await;
    batch(&mut admin, &format!("ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=120")).await;
    let fixture_pool = Pool::new(config(port, &database, &password, "activity-fixture"), None);
    let mut setup = fixture_pool.connect(SessionKind::User).await.unwrap();
    batch(&mut setup,"CREATE TABLE dbo.activity_fixture(id int NOT NULL PRIMARY KEY,note nvarchar(200) NOT NULL); INSERT dbo.activity_fixture VALUES(1,N'initial')").await;
    drop(setup);
    // A different Culprit connection can be the cause; only the observing session itself is excluded.
    let culprit_pool = Pool::new(config(port, &database, &password, "culprit"), None);
    let mut blocker = culprit_pool.connect(SessionKind::User).await.unwrap();
    let blocker_id = session_id(&mut blocker).await;
    let previous = "BEGIN TRAN; UPDATE dbo.activity_fixture SET note=N'idle-previous-canary' WHERE id=1;";
    batch(&mut blocker, previous).await;
    let mut waiter = fixture_pool.connect(SessionKind::User).await.unwrap();
    batch(&mut waiter, "SET LOCK_TIMEOUT 30000").await;
    let waiter_id = session_id(&mut waiter).await;
    // A supplementary character before the active statement proves byte offsets are not Unicode character offsets.
    let current = "UPDATE dbo.activity_fixture SET note=N'blocked-current-canary' WHERE id=1 OPTION(RECOMPILE)";
    let workload = format!("SELECT N'🌟' AS prefix;\n{current};");
    let waiting = tokio::spawn(async move {
        let result = waiter.simple_query(workload).await.unwrap().into_results().await;
        (waiter, result)
    });
    let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
    let started = Instant::now();
    let policy = ActivityPolicy { blocked_after_ms: 1000, running_after_ms: 86_400_000 };
    let (mut reader, mut frame) = loop {
        assert!(started.elapsed() < Duration::from_secs(12), "the fixture never established a sustained blocker");
        let mut reader =
            ActivityReader::open(pool.clone(), options(ActivityScope::CurrentDatabase), "127.0.0.1".into(), port)
                .unwrap();
        let frame = reader.observe().await.unwrap();
        let established = frame.records.iter().any(|record| {
            record.facts.key.session.id == waiter_id
                && record.facts.conditions(&policy).contains(&ActivityCondition::Blocked)
        });
        if established {
            break (reader, frame);
        }
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let culprit_sessions = frame
        .records
        .iter()
        .filter(|record| {
            record.facts.client.as_ref().and_then(|client| client.application.as_deref()) == Some("culprit")
        })
        .map(|record| record.facts.key.session.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        culprit_sessions,
        vec![blocker_id.as_str()],
        "retain the other Culprit blocker, while excluding the observer by native session ID"
    );
    let evidence = ActivityEvidence::select(&frame.observations(), &policy).unwrap();
    let keys: BTreeSet<_> = evidence.observations.iter().map(|row| row.key.clone()).collect();
    assert!(
        evidence
            .conditions
            .iter()
            .any(|(key, conditions)| key.session.id == blocker_id
                && conditions.contains(&ActivityCondition::IdleBlocker))
    );
    assert!(evidence.blocking.iter().any(|edge| edge.waiter.session.id == waiter_id
        && edge.owner.session().is_some_and(|owner| owner.id == blocker_id)
        && edge.owner_observed));
    let at = chrono::DateTime::parse_from_rfc3339(&frame.context.at).unwrap();
    assert!((chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds().abs() < 10);
    let native_waiter = frame.records.iter().find(|record| record.facts.key.session.id == waiter_id).unwrap();
    let key = native_waiter.facts.key.clone();
    let cpu = native_waiter.facts.request_counters.as_ref().unwrap().cpu_ms.clone();
    assert!(!key.session.generation.ends_with('Z'));
    let blocker_age = frame
        .records
        .iter()
        .find(|record| record.facts.key.session.id == blocker_id)
        .unwrap()
        .facts
        .transaction_age_ms
        .as_ref()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(blocker_age < 60_000, "native local time was incorrectly interpreted as UTC");
    assert_eq!(reader.enrich(&mut frame, &keys, false, 16 * 1024).await.unwrap(), ActivityDetailStatus::Collected);
    let native_waiter = frame.records.iter().find(|record| record.facts.key.session.id == waiter_id).unwrap();
    assert_eq!(native_waiter.current_sql.text().unwrap().trim(), current);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let exported = crate::telemetry::activity::export_observations(frame.records);
    let exported_waiter = exported.iter().find(|row| row.key.session.id == waiter_id).unwrap();
    assert_eq!(exported_waiter.key, key);
    assert!(!serde_json::to_string(&exported).unwrap().contains("blocked-current-canary"));
    let exported_blocker = exported.iter().find(|row| row.key.session.id == blocker_id).unwrap();
    assert_eq!(exported_blocker.client.as_ref().unwrap().application.as_deref(), Some("culprit"));
    assert_eq!(exported_blocker.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    let live = crate::live::collect(pool.clone(), "127.0.0.1".into(), port).await.unwrap();
    assert!(live.notes.is_empty());
    let live_waiter = live.sessions.iter().find(|session| session.id == waiter_id).unwrap();
    assert_eq!(live_waiter.blocked_by.as_deref(), Some(blocker_id.as_str()));
    assert_eq!(live_waiter.sql.as_deref(), Some(current));
    assert!(live_waiter.cpu_ms.unwrap() >= cpu.as_ref().unwrap().parse::<f64>().unwrap());
    let live_blocker = live.sessions.iter().find(|session| session.id == blocker_id).unwrap();
    assert_eq!(live_blocker.sql.as_deref(), Some(previous));
    assert_eq!(live_blocker.application.as_deref(), Some("culprit"));
    // Complete the fixture and prove a later shared observation contains no old request identity.
    batch(&mut blocker, "COMMIT").await;
    let (waiter, result) = waiting.await.unwrap();
    result.unwrap();
    drop(waiter);
    drop(blocker);
    let after = crate::live::collect(pool.clone(), "127.0.0.1".into(), port).await.unwrap();
    assert!(!after.sessions.iter().any(|session| session.id == waiter_id || session.id == blocker_id));
    // The shared source returns only fully drained statistics sessions to the existing pool.
    let mut reused = pool.get().await.unwrap();
    assert_eq!(
        crate::conn::query_one(&mut reused, "SELECT /* culprit */ 1 AS value").await.unwrap().f64("value"),
        Some(1.0)
    );
    drop(reused);
    drop(pool);
    batch(&mut admin, &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE")).await;
    batch(&mut admin, &format!("DROP DATABASE [{database}]")).await;
}
