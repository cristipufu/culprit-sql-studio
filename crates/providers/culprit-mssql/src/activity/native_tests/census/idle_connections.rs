//! Explicit small/medium cost control with a genuine SqlClient MARS actor; no ordinary smoke admission.
use super::*;
use crate::read::{Parameter, ReadSession};
mod actor;
mod setup;
use crate::test_support::owned_case;
use setup::IdleClients;
const IDLE_POPULATIONS: [usize; 2] = [3, 32];
const ROUNDS: usize = 2;
const COST_CASE_BOUND: Duration = Duration::from_secs(90);
struct IdleIdentity {
    session: NativeActivityIdentity,
    connection_id: String,
    connected_without_offset: String,
}
const IDENTIFY_IDLE_SQL: &str = r#"SELECT /* culprit idle fixture identity */
 CONVERT(varchar(16),s.session_id) AS session_id,CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 DB_NAME(s.database_id) AS database_name,CONVERT(varchar(36),c.connection_id) AS connection_id,
 CONVERT(varchar(40),CAST(c.connect_time AS datetime2),126) AS connected_without_offset
 FROM sys.dm_exec_sessions s JOIN sys.dm_exec_connections c ON c.session_id=s.session_id
 AND c.most_recent_session_id=s.session_id AND c.parent_connection_id IS NULL
 WHERE s.session_id=@@SPID"#;

const WITNESS_SQL: &str = r#"SELECT TOP (5) /* culprit census fixture witness */
 CONVERT(varchar(16),s.session_id) AS session_id,CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 r.request_id,r.database_id AS request_database_id,s.database_id AS session_database_id,
 CONVERT(varchar(36),c.connection_id) AS connection_id,CONVERT(varchar(36),c.parent_connection_id) AS parent_connection_id,
 CONVERT(varchar(36),parent.connection_id) AS root_connection_id,c.net_transport,
 CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at,CONVERT(varchar(16),DB_ID()) AS selected_database_id
 FROM sys.dm_exec_sessions s JOIN sys.dm_exec_requests r ON r.session_id=s.session_id
 JOIN sys.dm_exec_connections c ON c.connection_id=r.connection_id AND c.session_id=s.session_id
 LEFT JOIN sys.dm_exec_connections parent ON parent.connection_id=c.parent_connection_id AND parent.parent_connection_id IS NULL AND parent.session_id=s.session_id AND parent.most_recent_session_id=s.session_id
 WHERE (s.session_id=@P1 AND s.login_time=CAST(CONVERT(datetime2,@P2,126) AS datetime))
 OR (s.session_id=@P3 AND s.login_time=CAST(CONVERT(datetime2,@P4,126) AS datetime))
 OPTION(MAXDOP 1)"#;

const IDLE_WITNESS_SQL: &str = r#"SELECT TOP(5) /* culprit idle MARS fixture witness */
 CONVERT(varchar(16),s.session_id) AS session_id,CAST(NULL AS int) AS request_id,
 CAST(NULL AS int) AS request_database_id,s.database_id AS session_database_id,
 CONVERT(varchar(36),c.connection_id) AS connection_id,CONVERT(varchar(36),c.parent_connection_id) AS parent_connection_id,
 CONVERT(varchar(36),CASE WHEN c.parent_connection_id IS NULL THEN c.connection_id ELSE parent.connection_id END) AS root_connection_id,c.net_transport,
 CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at,
 (SELECT COUNT(*) FROM sys.dm_exec_requests r WHERE r.session_id=s.session_id) AS current_requests
 FROM sys.dm_exec_sessions s JOIN sys.dm_exec_connections c ON c.session_id=s.session_id AND c.most_recent_session_id=s.session_id
 LEFT JOIN sys.dm_exec_connections parent ON parent.connection_id=c.parent_connection_id AND parent.parent_connection_id IS NULL AND parent.session_id=s.session_id
 WHERE s.session_id=@P1 AND s.login_time=CAST(CONVERT(datetime2,@P2,126) AS datetime) AND s.database_id=DB_ID()
 OPTION(MAXDOP 1)"#;

#[tokio::test]
#[ignore = "explicit nonce-owned MARS qualification only; requires pinned fixture SqlClient DLL; source budgets unchanged, five measured captures plus one sealed production capture and at most32 idle clients plus2 SqlClient connections"]
async fn owned_census_idle_connections_measure_small_medium_and_require_real_mars_and_foreign_request_witnesses()
-> Result<()> {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + COST_CASE_BOUND,
        &cancellation,
        AssertUnwindSafe(Box::pin(run(&directory, &cancellation))).catch_unwind(),
    )
    .await;
    save_cost_receipt(
        &directory,
        "idle-connection-census-outcome",
        &json!({"completed":!expired&&matches!(&outcome,Ok(Ok(()))),"expired":expired,"client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "source_cost_qualified":false,"planned_measured_source_captures":5,"planned_factory_captures":1,"fixture_actor_connection_cap":2,"fixture_actor_query_cap":5,"fixture_witness_query_cap":2,"scope":"small3/medium32 owned idle sessions plus genuine MARS/foreign-request controls; no500-row ceiling or Azure/MI qualification"}),
    )?;
    match outcome {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired);
    Ok::<(), culprit_core::Error>(())
}

async fn run(directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let fixture =
        Box::pin(owned_case::admitted(cancellation, OwnedSourceFixture::open("culprit-idle-connection-cost"))).await?;
    let mut captures =
        crate::test_support::owned_captures::OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port)?;
    owned_case::require_active(cancellation)?;
    let mut actor = actor::Actor::start(&fixture)?;
    let mut clients = IdleClients::new();
    let mut original = None;
    let outcome = AssertUnwindSafe(async {
        let identity = Box::pin(actor.ready(cancellation)).await?;
        let active_witness = Box::pin(witness(&fixture, &identity, directory, false, cancellation)).await?;
        save_cost_receipt(directory, "idle-connection-native-mars-witness", &active_witness)?;
        for population in IDLE_POPULATIONS {
            Box::pin(clients.extend_to(population, &fixture, cancellation)).await?;
            for round in 0..ROUNDS {
                Box::pin(measure(&fixture, directory, clients.identities(), &identity, population, round, false, None, cancellation)).await?;
            }
        }
        Box::pin(actor.idle(cancellation)).await?;
        let idle = Box::pin(witness(&fixture, &identity, directory, true, cancellation)).await?;
        let cached_children = idle["native_children"].as_array().ok_or_else(invalid_data)?;
        for original in active_witness["native_mars_requests"].as_array().ok_or_else(invalid_data)? {
            let child = original[1].as_str().ok_or_else(invalid_data)?;
            if !cached_children.iter().any(|cached| cached.as_str() == Some(child)) {
                return Err(invalid_data());
            }
        }
        let root = idle["native_root"].as_str().ok_or_else(invalid_data)?;
        save_cost_receipt(directory, "idle-connection-native-idle-mars-witness", &idle)?;
        Box::pin(measure(&fixture, directory, clients.identities(), &identity, 32, ROUNDS, true, Some(root), cancellation)).await?;
        owned_case::require_active(cancellation)?;
        let captured = Box::pin(captures.capture_with_cancellation(CollectionSelection::Activity, cancellation)).await?;
        if !(7..=8).contains(&captured.manifest.source_queries) { return Err(invalid_data()) }
        captured.with_activity_frame(|frame| {
            let report = culprit_checks::session_census::explain_frame(frame)?;
            let entry = report.entries.iter().find(|entry| entry.activity.session == identity.mars).ok_or_else(invalid_data)?;
            let id = entry.fields.iter().find(|field| field.key == "idle_connection_id").and_then(|field| field.value.as_deref());
            if id != Some(root) || !entry.idle || entry.activity.request.is_some() || entry.reference.is_none() { return Err(invalid_data()) }
            Ok(())
        })?;
        save_cost_receipt(directory, "idle-connection-factory-original", &json!({"manifest":captured.manifest,"records":captured.records,"source_cost_qualified":false,"source_queries_are_separate_from_five_measured_rounds":true}))?;
        original = Some(captured);
        Ok(())
    }).catch_unwind().await;
    // Query settlement and socket retirement are independent. Every retained client is awaited even after
    // identity failure, cancellation or panic; physical close cannot discharge an undrained identity query.
    clients.fence_unconfirmed(&fixture.pool);
    let idle_retirement = AssertUnwindSafe(Box::pin(clients.retire())).catch_unwind().await;
    let actor_retirement = AssertUnwindSafe(Box::pin(actor.finish())).catch_unwind().await;
    let needs_supervisor = !matches!(&idle_retirement, Ok(Ok(())))
        || !matches!(&actor_retirement, Ok(Ok(_)))
        || fixture.pool.source_shutdown() != SourceShutdown::Quiescent;
    let saved = save_cost_receipt(
        directory,
        "idle-connection-actor-retirement",
        &json!({
            "retired":actor_retirement.as_ref().ok().and_then(|result|result.as_ref().ok()),
            "error":actor_retirement.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "actor_diagnostics":actor.diagnostics(),"fixture_admission_elapsed_ms":fixture.admission_elapsed_ms,
            "idle_clients":clients.accounting(),"fixture_pool_shutdown":shutdown_state(fixture.pool.source_shutdown()),
            "supervisor_destruction_required":needs_supervisor,
            "physical_retirement_does_not_confirm_query_cancellation":true,
        }),
    );
    match outcome {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    match idle_retirement {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    match actor_retirement {
        Ok(result) => {
            result?;
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
    saved?;
    let retained = original.ok_or_else(invalid_data)?;
    Box::pin(captures.verify_retained(&retained)).await?;
    Ok(())
}

async fn witness(
    fixture: &OwnedSourceFixture,
    identity: &actor::Identity,
    directory: &Path,
    idle: bool,
    cancellation: &CollectionCancellation,
) -> Result<serde_json::Value> {
    owned_case::require_active(cancellation)?;
    let mut reader = ReadSession::new(
        fixture.pool.clone(),
        CollectionBudget { max_queries: 1, statement_timeout_ms: 1000, total_timeout_ms: 5000, ..Default::default() },
    );
    let sql = if idle { IDLE_WITNESS_SQL } else { WITNESS_SQL };
    let result = AssertUnwindSafe(until_cancelled(
        cancellation,
        Box::pin(async {
            reader
                .query(
                    sql,
                    vec![
                        Parameter::Integer(identity.mars.id.parse().map_err(|_| invalid_data())?),
                        Parameter::Text(identity.mars.generation.clone()),
                        Parameter::Integer(identity.foreign.id.parse().map_err(|_| invalid_data())?),
                        Parameter::Text(identity.foreign.generation.clone()),
                    ],
                    5,
                )
                .await
                .map_err(crate::read::ReadError::into_error)
        }),
    ))
    .catch_unwind()
    .await;
    let shutdown = reader.close().await;
    let rows = match result {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    save_cost_receipt(
        directory,
        if idle { "idle-connection-native-idle-witness-rows" } else { "idle-connection-native-witness-rows" },
        &json!({"setup_queries":reader.queries(),"possible_setup_queries":ReadSession::SESSION_SETUP_QUERIES,"rows":rows.iter().map(|row|json!({
        "session_id":crate::read::optional_text(row,"session_id").ok(),"request_id":crate::read::optional_counter(row,"request_id").ok(),
        "request_database_id":crate::read::optional_counter(row,"request_database_id").ok(),"session_database_id":crate::read::optional_counter(row,"session_database_id").ok(),
        "connection_id":crate::read::optional_text(row,"connection_id").ok(),"parent_connection_id":crate::read::optional_text(row,"parent_connection_id").ok(),
        "root_connection_id":crate::read::optional_text(row,"root_connection_id").ok(),"net_transport":crate::read::optional_text(row,"net_transport").ok(),"observed_at":crate::read::optional_text(row,"observed_at").ok()
    })).collect::<Vec<_>>()}),
    )?;
    if idle {
        let mut roots = BTreeSet::new();
        let mut children = BTreeSet::new();
        for row in &rows {
            if crate::read::integer(row, "current_requests")? != 0 || text(row, "session_id")? != identity.mars.id {
                return Err(invalid_data());
            }
            let root = text(row, "root_connection_id")?;
            roots.insert(root.clone());
            if let Some(parent) = crate::read::optional_text(row, "parent_connection_id")? {
                if parent != root || text(row, "net_transport")? != "Session" {
                    return Err(invalid_data());
                }
                children.insert(text(row, "connection_id")?);
            }
        }
        // SqlClient also caches the earlier identity command's MARS session. Require both original
        // active children (checked by the caller), plus exactly one common parent, rather than
        // assuming the connection has only the two currently exercised logical sessions.
        if roots.len() != 1 || children.len() < 2 || rows.len() != children.len() + 1 {
            return Err(invalid_data());
        }
        return Ok(
            json!({"setup_queries":reader.queries(),"native_root":roots.first().unwrap(),"native_children":children,"no_current_requests":true}),
        );
    }
    let mut mars = BTreeSet::new();
    let mut parents = BTreeSet::new();
    let mut foreign = 0;
    for row in &rows {
        if text(row, "session_id")? == identity.mars.id {
            mars.insert((crate::read::integer(row, "request_id")?, text(row, "connection_id")?));
            let parent = text(row, "parent_connection_id")?;
            if text(row, "root_connection_id")? != parent {
                return Err(invalid_data());
            }
            parents.insert(parent);
            if text(row, "net_transport")? != "Session" {
                return Err(invalid_data());
            }
        } else if text(row, "session_id")? == identity.foreign.id {
            let selected = text(row, "selected_database_id")?.parse::<i64>().map_err(|_| invalid_data())?;
            if crate::read::integer(row, "request_database_id")? == selected
                || crate::read::integer(row, "session_database_id")? != selected
            {
                return Err(invalid_data());
            }
            foreign += 1;
        } else {
            return Err(invalid_data());
        }
    }
    if rows.len() != 3 || mars.len() != 2 || parents.len() != 1 || foreign != 1 {
        return Err(invalid_data());
    }
    Ok(
        json!({"setup_queries":reader.queries(),"shutdown":shutdown_state(shutdown),"native_mars_requests":mars,"native_parent_ids":parents,"foreign_current_request":foreign,"actual_source_budget_charged":false,"observer_work_separate":true}),
    )
}

async fn measure(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    identities: &[IdleIdentity],
    controls: &actor::Identity,
    population: usize,
    round: usize,
    controls_idle: bool,
    root: Option<&str>,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let budget = CollectionBudget {
        max_queries: 7,
        max_snapshot_rows: 500,
        page_rows: 500,
        statement_timeout_ms: 1000,
        total_timeout_ms: 5000,
        ..Default::default()
    };
    let io = Box::pin(owned_case::admitted(
        cancellation,
        crate::test_support::measured_read(
            fixture.pool.clone(),
            fixture.port,
            fixture.password(),
            &fixture.database,
            budget.clone(),
        ),
    ))
    .await?;
    let mut reader = ActivityReader::new(
        io,
        ReadOptions { budget, identity: IdentityDetail::Endpoint },
        "127.0.0.1".into(),
        fixture.port,
    );
    let work = async {
        let pending = capture::PendingActivityFrame::begin_census(&mut reader).await?;
        let completed = pending.finish(&mut reader).await?;
        ActivityCensusRead::new(completed.description, completed.frame.observations(), true)
    };
    let result = AssertUnwindSafe(until_cancelled(cancellation, Box::pin(work))).catch_unwind().await;
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    let metrics = reader.read_metrics();
    let saved = save_cost_receipt(
        directory,
        &format!("idle-connection-cost-{population}-{round}"),
        &json!({"population":population,"round":round,"source_queries":reader.query_count(),"source_accounting":reader.io.report(),"source_scope":reader.io.scope(),"metrics":metrics,"shutdown":shutdown_state(shutdown),"quiet":quiet.as_ref().ok(),"source_cost_qualified":false,"error":result.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code)}),
    );
    let read = match result {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if shutdown != SourceShutdown::Quiescent || !quiet? {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    saved?;
    if reader.io.report()["measurement_complete"] != true
        || !metrics.as_ref().is_some_and(|metrics| {
            metrics.is_valid()
                && !metrics.has_unconfirmed_work()
                && metrics.active_tasks == 0
                && metrics.untracked_tasks == 0
                && metrics.native_session_work.is_some()
                && metrics.templates.iter().all(|template| {
                    template.failures == 0
                        && template.timeouts == 0
                        && template.cancellations == 0
                        && template.unconfirmed_cancellations == 0
                })
        })
        || reader.query_count() != 7
    {
        return Err(invalid_data());
    }
    for identity in identities {
        let key = &identity.session;
        let mut found = read.observations.iter().filter(|row| row.key.session == *key);
        let row = found.next().ok_or_else(invalid_data)?;
        if found.next().is_some() || !row.idle || row.key.request.is_some() {
            return Err(invalid_data());
        }
        let Some(SessionNativeContext::Mssql(native)) =
            row.session_context.as_deref().and_then(|context| context.native.as_deref())
        else {
            return Err(invalid_data());
        };
        if native.request_connection_id.is_some() {
            return Err(invalid_data());
        }
        let Some(MssqlIdleSessionConnection::Observed { connection_id, connected_without_offset }) =
            native.idle_connection.as_ref()
        else {
            return Err(invalid_data());
        };
        if connection_id != &identity.connection_id || connected_without_offset != &identity.connected_without_offset {
            return Err(invalid_data());
        }
    }
    let mars = read.observations.iter().filter(|row| row.key.session == controls.mars).collect::<Vec<_>>();
    if controls_idle {
        let [row] = mars.as_slice() else { return Err(invalid_data()) };
        let Some(SessionNativeContext::Mssql(native)) =
            row.session_context.as_deref().and_then(|context| context.native.as_deref())
        else {
            return Err(invalid_data());
        };
        let Some(MssqlIdleSessionConnection::Observed { connection_id, .. }) = native.idle_connection.as_ref() else {
            return Err(invalid_data());
        };
        if !row.idle
            || row.key.request.is_some()
            || native.request_connection_id.is_some()
            || Some(connection_id.as_str()) != root
        {
            return Err(invalid_data());
        }
        return Ok(());
    }
    let foreign = read.observations.iter().find(|row| row.key.session == controls.foreign).ok_or_else(invalid_data)?;
    if mars.len() != 2
        || mars.iter().any(|row| row.key.request.is_none() || row.idle)
        || foreign.idle
        || foreign.key.request.is_some()
    {
        return Err(invalid_data());
    }
    for row in mars.into_iter().chain(std::iter::once(foreign)) {
        let Some(SessionNativeContext::Mssql(native)) =
            row.session_context.as_deref().and_then(|context| context.native.as_deref())
        else {
            return Err(invalid_data());
        };
        if native.idle_connection.is_some() {
            return Err(invalid_data());
        }
    }
    Ok(())
}

#[test]
fn actual_census_cost_case_future_stays_below_four_kibibytes_without_opening_a_database() {
    let cancellation = CollectionCancellation::default();
    let work = run(Path::new("."), &cancellation);
    assert!(std::mem::size_of_val(&work) <= 4096, "native case future embeds excessive owned work");
}
