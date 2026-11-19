//! Native mapping and scope proof only. The selected shared maps are not owned by the fixture's temporary
//! object, and its WAITFOR request is neither a page-latch contributor nor a directly observed holder.
use super::{
    fixture::{self, Fixture, invalid},
    workload::CompletedWorker,
};
use crate::{
    activity::{
        ActivityReader, IdentityDetail, ReadOptions,
        capture::{ActivityAuthorityPolicy, PendingActivityFrame},
        page_targets::{PageAddress, PageWaitGuard, chronological, context, mapped_target},
    },
    conn::map_error,
    read::{integer, signed_integer, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeSet, time::Duration};
use tokio::sync::oneshot;

const APPLICATION: &str = "page-fixture-shared-tempdb-scope";
const READY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_READINESS_OBSERVATIONS: usize = 4;
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const APPLICATION_READ_QUERIES: u32 = 7;
const MAPS: [(u32, PageWaitTargetKind); 3] =
    [(1, PageWaitTargetKind::Pfs), (2, PageWaitTargetKind::Gam), (3, PageWaitTargetKind::Sgam)];
const ACTOR_SETUP_SQL: &str = r#"
SET NOCOUNT ON;
SET LOCK_TIMEOUT 1000;
CREATE TABLE #culprit_shared_tempdb_mapping(id int NOT NULL);
INSERT #culprit_shared_tempdb_mapping VALUES(1);
SELECT /* culprit */ CONVERT(varchar(16),s.session_id) AS session_id,
 CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 DB_ID() AS application_database_id,DB_NAME() AS application_database,
 OBJECT_ID(N'tempdb..#culprit_shared_tempdb_mapping') AS temporary_object_id,
 (SELECT COUNT_BIG(*) FROM #culprit_shared_tempdb_mapping) AS temporary_rows
FROM sys.dm_exec_sessions s WHERE s.session_id=@@SPID OPTION(MAXDOP 1);
"#;
const ACTOR_WAIT_SQL: &str = r#"
WAITFOR DELAY '00:00:08';
DROP TABLE #culprit_shared_tempdb_mapping;
SELECT /* culprit */ CONVERT(bigint,1) AS completed;
"#;

struct Actor {
    session: NativeActivityIdentity,
    database_id: u32,
    database: String,
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_shared_tempdb_allocation_maps_keep_the_observed_request_in_its_application_database() {
    fixture::run(|fixture| {
        Box::pin(async move {
            let actor = start_actor(fixture).await?;
            let (request, application_queries) = observe_application_request(fixture, &actor).await?;
            let targets = map_shared_allocation_pages(fixture, &actor).await?;
            let completed = fixture.finish_workload().await?;
            if completed.len() != 1 || completed[0].pattern != 0 || completed[0].inserts != 1 {
                return Err(invalid("the single temporary-object actor did not complete its exact one-row fixture"));
            }
            assert_eq!(request.database.as_deref(), Some(actor.database.as_str()));
            assert_eq!(request.key.session, actor.session);
            assert!(targets.iter().all(|target| target.header.database_id == 2));
            println!(
                "{}",
                serde_json::json!({
                    "proof":"native-shared-tempdb-allocation-map-and-application-request-scope",
                    "shared_map_pages":targets.len(),"header_reads":2,"mapping_native_queries":14,
                    "application_native_queries":application_queries,
                    "application_readiness_observations":application_queries / APPLICATION_READ_QUERIES,
                    "actor_native_commands":2,"owned_temporary_rows":1,
                    "observed_page_latch_contribution":false,"direct_holder_proof":false,
                    "native_cpu_io_cost_qualified":false,"durability_or_warehouse_proof":false,
                })
            );
            Ok(())
        })
    })
    .await;
}

async fn start_actor(fixture: &mut Fixture) -> Result<Actor> {
    let mut client = tokio::time::timeout(READY_TIMEOUT, fixture.client(APPLICATION))
        .await
        .map_err(|_| invalid("native temporary-object connection exceeded its setup bound"))??;
    let (ready, receive) = oneshot::channel();
    // The fixture retains the original handle before readiness can be cancelled. Its existing cleanup aborts
    // and joins this actor, then removes the nonce application database; no detached SQL worker is introduced.
    fixture.workers.push(Some(tokio::spawn(async move {
        // Drain setup through its final DONE before starting the wait in another command. A SELECT followed
        // by WAITFOR in one TDS batch can remain buffered until that wait ends; it cannot establish readiness.
        let setup =
            client.simple_query(ACTOR_SETUP_SQL).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
        let mut rows = setup.iter().flatten();
        let original = rows.next().ok_or_else(|| invalid("native fixture setup returned no identity"))?;
        let actor = Actor::decode(&Rec::from_row(original))?;
        if rows.next().is_some() {
            return Err(invalid("native fixture setup returned more than its one identity row"));
        }
        ready.send(actor).map_err(|_| invalid("native fixture setup was abandoned"))?;
        let waited =
            client.simple_query(ACTOR_WAIT_SQL).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
        let mut rows = waited.iter().flatten();
        let completed = rows.next().ok_or_else(|| invalid("native fixture returned no completion"))?;
        let completed = integer(&Rec::from_row(completed), "completed")?;
        if rows.next().is_some() {
            return Err(invalid("native fixture returned more than one completion row"));
        }
        client.close().await.map_err(map_error)?;
        if completed != 1 {
            return Err(invalid("native fixture ended without its original one-row completion"));
        }
        Ok(CompletedWorker { pattern: 0, inserts: 1 })
    })));
    tokio::time::timeout(READY_TIMEOUT, receive)
        .await
        .map_err(|_| invalid("native temporary-object actor did not become ready within its bound"))?
        .map_err(|_| invalid("native temporary-object actor failed before readiness"))
}

impl Actor {
    fn decode(row: &Rec) -> Result<Self> {
        let session = NativeActivityIdentity {
            id: text(row, "session_id")?,
            generation: crate::sql_time::native_time(&text(row, "session_generation")?)?,
        };
        let database_id = u32::try_from(integer(row, "application_database_id")?)
            .map_err(|_| invalid("invalid native application database ID"))?;
        if !session.is_valid()
            || database_id <= 4
            || signed_integer(row, "temporary_object_id")? == 0
            || integer(row, "temporary_rows")? != 1
        {
            return Err(invalid("native actor did not establish its application scope and one temporary row"));
        }
        Ok(Self { session, database_id, database: text(row, "application_database")? })
    }
}

#[test]
fn native_temporary_object_ids_can_be_negative_without_losing_application_scope() {
    use crate::value::Val;
    let row = Rec::of(&[
        ("session_id", Val::Text("51".into())),
        ("session_generation", Val::Text("2026-11-19T12:00:00.003".into())),
        ("application_database_id", Val::Int(7)),
        ("application_database", Val::Text("owned_application".into())),
        ("temporary_object_id", Val::Int(-12345)),
        ("temporary_rows", Val::Int(1)),
    ]);
    let actor = Actor::decode(&row).unwrap();
    assert_eq!(actor.database_id, 7);
    assert_eq!(actor.database, "owned_application");
    let absent = crate::resources::test_support::replace(&row, &[("temporary_object_id", Val::Int(0))]);
    assert!(Actor::decode(&absent).is_err());
}

fn reader(fixture: &Fixture, readiness_remaining: Option<Duration>) -> Result<ActivityReader> {
    let mut budget = crate::page_waits::bounded_budget(&CollectionBudget::default());
    if let Some(remaining) = readiness_remaining {
        let milliseconds = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
        budget.total_timeout_ms = budget.total_timeout_ms.min(milliseconds);
        budget.statement_timeout_ms = budget.statement_timeout_ms.min(budget.total_timeout_ms);
    }
    ActivityReader::open(
        fixture.pool.as_ref().ok_or_else(|| invalid("owned application pool was absent"))?.clone(),
        ReadOptions { budget, identity: IdentityDetail::Endpoint },
        "127.0.0.1".into(),
        fixture.port,
    )
}

async fn observe_application_request(fixture: &Fixture, actor: &Actor) -> Result<(ActivityObservation, u32)> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    let mut queries = 0;
    for _ in 0..MAX_READINESS_OBSERVATIONS {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.as_millis() == 0 {
            break;
        }
        let mut reader = reader(fixture, Some(remaining))?;
        let observed = async {
            let pending =
                PendingActivityFrame::begin(&mut reader, ActivityAuthorityPolicy::AdministrativeCandidates).await?;
            pending.require_complete()?;
            let completed = pending.finish(&mut reader).await?;
            if !completed.action_context.as_ref().is_some_and(NativeReadContext::has_qualified_local_owner) {
                return Err(invalid("native readiness did not establish the original application's source authority"));
            }
            let frame = completed.frame;
            if frame.context.database.database != actor.database {
                return Err(invalid("native readiness observation changed its application database"));
            }
            let selected: Vec<_> = frame.records.iter().filter(|row| row.facts.key.session == actor.session).collect();
            let request = match selected.as_slice() {
                [] => return Ok(None),
                [record] => &record.facts,
                _ => return Err(invalid("native readiness duplicated the original application actor")),
            };
            if request.key.request.is_none() {
                return Ok(None);
            }
            if !request.has_session_generation_authority()
                || request.database.as_deref() != Some(actor.database.as_str())
                || request.client.as_ref().and_then(|client| client.application.as_deref()) != Some(APPLICATION)
            {
                return Err(invalid("the observed original request escaped its native application scope"));
            }
            Ok((request.wait.as_ref().and_then(|wait| wait.native_type.as_deref()) == Some("WAITFOR"))
                .then(|| request.clone()))
        }
        .await;
        let observed = settle_reader(&mut reader, observed, APPLICATION_READ_QUERIES).await?;
        queries += reader.query_count();
        if let Some(request) = observed {
            return Ok((request, queries));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL.min(remaining)).await;
    }
    Err(invalid("finite native readiness observations did not establish the original actor's active WAITFOR"))
}

async fn map_shared_allocation_pages(fixture: &Fixture, actor: &Actor) -> Result<Vec<MappedPageTarget>> {
    let mut reader = reader(fixture, None)?;
    let mapped = async {
        let initial = context::initial_scan(&mut reader, true).await?;
        if initial.authority.database_id != actor.database_id
            || initial.authority.activity.database.database != actor.database
            || initial.unavailable_for(PageAddress { database: 2, file: 1, page: 1 }).is_some()
        {
            return Err(invalid(
                "shared tempdb mapping authority or the independent application scope was unavailable",
            ));
        }
        let addresses =
            MAPS.iter().map(|(page, _)| PageAddress { database: 2, file: 1, page: *page }).collect::<BTreeSet<_>>();
        let before = reader.page_headers(&addresses, &initial).await?;
        let after = reader.page_headers(&addresses, &initial).await?;
        let mut checked = context::read(&mut reader, &initial).await?;
        if !initial.same_source(&checked) {
            return Err(invalid("native application or shared tempdb authority changed during the page reads"));
        }
        let shared = checked.shared.as_ref().ok_or_else(|| invalid("native shared tempdb authority was absent"))?;
        let resource =
            initial.authority.source_instance().ok_or_else(|| invalid("native instance owner was absent"))?;
        let source = shared.read_context()?;
        if source.target != CollectionTarget::Shared(resource.clone())
            || !source.has_qualified_local_owner()
            || source.database_writable.is_some()
        {
            return Err(invalid("shared maps borrowed an application role or lacked their native instance owner"));
        }
        let mut targets = Vec::new();
        for (page, kind) in MAPS {
            let address = PageAddress { database: 2, file: 1, page };
            let first = before.get(&address).ok_or_else(|| invalid("native initial allocation header was absent"))?;
            let last = after.get(&address).ok_or_else(|| invalid("native final allocation header was absent"))?;
            if !address.matches(&first.header)?
                || !address.matches(&last.header)?
                || !first.same_mapping(last)
                || !chronological(&[
                    &source.observed_from,
                    &first.observed_at,
                    &last.observed_at,
                    &source.observed_until,
                ])
            {
                return Err(invalid(&format!(
                    "native shared map 2:1:{page} lacked a stable qualified file/header/clock witness"
                )));
            }
            let target = mapped_target(&checked, last)?;
            let expected_basis = if last.catalog.file_guid.is_some() {
                PageTargetIdentityBasis::LocalDatabaseFileEpoch
            } else {
                PageTargetIdentityBasis::LocalTempdbPrimaryFileEpoch
            };
            if target.kind != kind
                || target.object.is_some()
                || target.identity_basis != expected_basis
                || !target.is_valid()
                || !matches!(&target.scope, PageTargetScope::SharedResource { context } if context.as_ref() == &source)
            {
                return Err(invalid("native shared map was not its independently scoped PFS, GAM or SGAM target"));
            }
            checked
                .shared
                .as_mut()
                .ok_or_else(|| invalid("native shared tempdb authority was lost"))?
                .retain_file(last.file_epoch.as_ref().ok_or_else(|| invalid("native file epoch was absent"))?)?;
            targets.push(target);
        }
        if !reader.verify_page_wait_context(&PageWaitGuard(checked)).await? {
            return Err(invalid(
                "native shared tempdb database, selected file or process epoch failed final verification",
            ));
        }
        Ok(targets)
    }
    .await;
    settle_reader(&mut reader, mapped, 14).await
}

async fn settle_reader<T>(reader: &mut ActivityReader, result: Result<T>, expected_queries: u32) -> Result<T> {
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics().ok_or_else(|| invalid("native page reader metrics were absent"))?;
    if shutdown != SourceShutdown::Quiescent
        || metrics.has_unconfirmed_work()
        || metrics.untracked_tasks != 0
        || metrics.templates.iter().any(|template| template.failures != 0 || template.timeouts != 0)
    {
        eprintln!("native shared tempdb mapping result={:?}, shutdown={shutdown:?}", result.as_ref().err());
        return Err(invalid("native shared tempdb reader did not settle every original read owner"));
    }
    let value = result?;
    if reader.query_count() != expected_queries {
        return Err(invalid("native shared tempdb test escaped its exact bounded read population"));
    }
    Ok(value)
}
