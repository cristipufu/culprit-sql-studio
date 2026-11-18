//! Three owned, idle connections establish session visibility, not transport counts or a cost qualification.
use super::super::*;
use crate::{
    conn::{MsClient, SessionKind},
    read::measurement::{MeasuredRead, shutdown_state},
    test_support::{
        OwnedSourceFixture,
        owned_case::{finish_case, until_cancelled},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

const OWNED_IDLE_SESSIONS: usize = 3;
const SOURCE_QUERY_LIMIT: u32 = 7;
const CASE_TIMEOUT: Duration = Duration::from_secs(60);
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const IDENTIFY_OWNED_SESSION: &str = r#"SELECT /* culprit census fixture */
 CONVERT(varchar(16),s.session_id) AS session_id,
 CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS session_generation,
 DB_NAME(s.database_id) AS database_name
 FROM sys.dm_exec_sessions s WHERE s.session_id=@@SPID"#;

#[tokio::test]
#[ignore = "requires the explicit nonce-owned loopback MSSQL fixture and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select activity::native_tests::census explicitly; holds three idle clients; no DDL, DML or planted application work"]
async fn owned_selected_database_census_retains_idle_generations_without_sql_enrichment_and_closes_same_owner() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(run(&directory, &cancellation)).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory, "session-census-native-outcome", &json!({
        "case_deadline_exceeded":expired,"completed":!expired && matches!(&outcome,Ok(Ok(()))),
        "cancellation_requested":cancellation.is_cancelled(),
        "full_case_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "native_cpu_io_cost_qualified":false,"production_admission":"unqualified",
        "cleanup":"same source owner closes or fences; all owned idle clients are dropped before outcome; supervisor owns nonce database/container teardown",
        "unexercised":"MARS, pool reset/prelogin and active exact-template cancellation are not established by three idle Tiberius clients",
    })).expect("private census outcome receipt");
    match outcome {
        Ok(result) => result.expect("actual native idle session census"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired, "a late settled result is not timely qualification");
}

async fn run(directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let fixture = OwnedSourceFixture::open("culprit-session-census").await?;
    let mut actors = Vec::with_capacity(OWNED_IDLE_SESSIONS);
    let mut identities = Vec::with_capacity(OWNED_IDLE_SESSIONS);
    for _ in 0..OWNED_IDLE_SESSIONS {
        if cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        actors.push(
            tokio::time::timeout(SETUP_TIMEOUT, fixture.pool.connect(SessionKind::User))
                .await
                .map_err(|_| CollectionStop::TimedOut.error())??,
        );
        let identity = identify(actors.last_mut().ok_or_else(invalid_data)?, &fixture.database).await?;
        identities.push(identity);
    }
    if identities.iter().collect::<BTreeSet<_>>().len() != OWNED_IDLE_SESSIONS {
        return Err(invalid_data());
    }
    let budget = CollectionBudget {
        max_snapshot_rows: 256,
        max_queries: SOURCE_QUERY_LIMIT,
        statement_timeout_ms: 1500,
        total_timeout_ms: 12_000,
        page_bytes: 8 * 1024 * 1024,
        ..Default::default()
    };
    let io =
        MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget.clone())
            .await?;
    let options = ReadOptions { budget, identity: IdentityDetail::Endpoint };
    let mut reader = ActivityReader::new(io, options, "127.0.0.1".into(), fixture.port);
    let outcome =
        AssertUnwindSafe(until_cancelled(cancellation, capture_idle(&mut reader, &identities))).catch_unwind().await;
    // The owning reader survives interrupted borrowed futures, errors and panic until this confirmed close.
    let shutdown = reader.close().await;
    actors.clear();
    let quiet = reader.io.is_quiet().await;
    let saved = save_cost_receipt(
        directory,
        "session-census-native-source",
        &json!({
            "scope":reader.io.scope(),"accounting":reader.io.report(),"source_queries":reader.query_count(),
            "owned_idle_session_count":identities.len(),"owned_exact_native_generations":identities,
            "setup_identity_queries":OWNED_IDLE_SESSIONS,"setup_outside_source_measurement":true,
            "captured_source":outcome.as_ref().ok().and_then(|result|result.as_ref().ok()).map(|read|&read.source),
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "error_detail":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).and_then(|error|error.detail),
            "panicked":outcome.is_err(),"shutdown":shutdown_state(shutdown),
            "independently_observed_quiet":quiet.as_ref().ok(),"owned_clients_retired":actors.is_empty(),
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
            "limitations":"actual owned endpoint compatibility only; no Azure/MI qualification, no no-grant or server-cost acceptance; returned row caps do not bound server enumeration; active exact-template cancellation unexercised",
        }),
    );
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    result?;
    saved?;
    if !quiet? {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let metrics = reader.read_metrics().ok_or_else(invalid_data)?;
    let metadata_template =
        digest(sql::metadata(IdentityDetail::Endpoint, reader.request_page_server_read_capability).as_bytes());
    if !metrics.templates.iter().any(|template| {
        template.template_id == metadata_template
            && template.completed_tasks == 1
            && template.failures == 0
            && template.rows_decoded.parse::<usize>().is_ok_and(|rows| rows >= OWNED_IDLE_SESSIONS)
    }) {
        return Err(invalid_data());
    }
    if metrics
        .templates
        .iter()
        .any(|template| template.template_id == digest(sql::details(true, sql::QueryPolicy::Activity).as_bytes()))
    {
        return Err(invalid_data());
    }
    Ok(())
}

async fn identify(client: &mut MsClient, database: &str) -> Result<NativeActivityIdentity> {
    let row = tokio::time::timeout(SETUP_TIMEOUT, crate::conn::query_one(client, IDENTIFY_OWNED_SESSION))
        .await
        .map_err(|_| CollectionStop::TimedOut.error())?
        .map_err(crate::conn::map_error)?;
    if text(&row, "database_name")? != database {
        return Err(invalid_data());
    }
    Ok(NativeActivityIdentity {
        id: text(&row, "session_id")?,
        generation: crate::sql_time::native_time(&text(&row, "session_generation")?)?,
    })
}

async fn capture_idle(
    reader: &mut ActivityReader<MeasuredRead>,
    identities: &[NativeActivityIdentity],
) -> Result<ActivityCensusRead> {
    let pending =
        capture::PendingActivityFrame::begin(reader, capture::ActivityAuthorityPolicy::RetainPartialBaseline).await?;
    let completed = pending.finish(reader).await?;
    let read = ActivityCensusRead::new(completed.description, completed.frame.observations(), true)?;
    for identity in identities {
        let mut matching = read.observations.iter().filter(|row| &row.key.session == identity);
        let Some(row) = matching.next() else { return Err(invalid_data()) };
        let context = row.session_context.as_deref().ok_or_else(invalid_data)?;
        if matching.next().is_some()
            || !row.idle
            || row.key.request.is_some()
            || row.request_counters.is_some()
            || row.open_transactions != Some(0)
            || row.scope != ActivityScopeBasis::SessionDatabase
            || context.lifecycle != SessionLifecycle::Idle
            || context.client_library.is_none()
            || !row.conditions(&ActivityPolicy::default()).is_empty()
        {
            return Err(invalid_data());
        }
    }
    Ok(read)
}
