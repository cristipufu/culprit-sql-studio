//! A real original capture survives native workers ending; a later lookup cannot replace the incident.
use super::{
    durable_capture::{Captured, OwnedCaptures, require_readable_capture},
    fixture::{self, Fixture, invalid},
    workload::{WRITER_APPLICATIONS, WRITERS_PER_PATTERN},
};
use culprit_core::{PageWaitActivityEvidence, PageWaitReport, Result, SourceOutcome, WaitResourceKind, telemetry::*};
use std::time::{Duration, Instant};

const MAX_BASELINES: usize = 8;
const BASELINE_INTERVAL: Duration = Duration::from_millis(10);
// Admin+seed session SET2, ownership1, DB setup1, seed4, sixteen actor SETs/commands32, DB cleanup1.
const FIXTURE_DISPATCH_CAP: u32 = 41;

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn a_retained_native_baseline_survives_work_ending_before_its_focused_lookup() {
    fixture::run(|fixture| Box::pin(prove(fixture))).await;
}

async fn prove(fixture: &mut Fixture) -> Result<()> {
    let directory = crate::test_support::receipt_directory();
    let mut captures = OwnedCaptures::new(fixture)?;
    let preparing = Instant::now();
    let work = fixture.start_workload(false, false).await?;
    let preparation_ms = preparing.elapsed().as_secs_f64() * 1000.0;
    let mut selected = None;
    let mut baseline_reads = 0;
    let mut baseline_total_allowance = 0;
    let mut baseline_captures = 0;
    for _ in 0..MAX_BASELINES {
        if fixture.workers_finished() {
            break;
        }
        let captured = captures.capture(CollectionSelection::Activity).await?;
        require_readable_capture(&captured)?;
        baseline_captures += 1;
        baseline_reads += captured.manifest.source_queries;
        baseline_total_allowance += captured.manifest.request.budget.max_queries;
        if let Some(focus) = select_original(&captured)? {
            captures.verify_retained(&captured).await?;
            crate::test_support::save_cost_receipt(
                &directory,
                "ended-page-wait-original-before-retirement",
                &serde_json::json!({
                    "original_manifest":captured.manifest,"original_row":captured.records[focus.participant.parent.row_ordinal as usize],
                    "original_reference":focus.participant.parent,"original_focus":focus,
                    "native_latch_holder_proved":false,"page_aba_excluded":false,"native_cost_qualified":false,
                }),
            )?;
            selected = Some((captured, focus));
            break;
        }
        tokio::time::sleep(BASELINE_INTERVAL).await;
    }
    let completed = fixture.finish_workload().await?;
    let work_ms = work.elapsed().as_secs_f64() * 1000.0;
    let (original, focus) = selected.ok_or_else(|| {
        invalid("finite native work exposed no complete original page-latch Activity capture; no incident proof")
    })?;
    let detail = captures.capture(CollectionSelection::FocusedPageWait { focus: Box::new(focus.clone()) }).await?;
    require_readable_capture(&detail)?;
    let source = detail.source()?;
    let SourceObservation::FocusedPageWait { snapshot, result } = &source.observation else {
        return Err(invalid("native followup lost its original typed focus"));
    };
    require_ended_focus(&focus, result, snapshot.records, detail.records.len())?;
    let enrichment_observed = enrichment_observed(
        detail.manifest.read_metrics.as_ref().ok_or_else(|| invalid("native read metrics absent"))?,
    );
    if enrichment_observed == Some(true) {
        return Err(invalid("ended focus performed header or SQL enrichment"));
    }
    let report = original.with_activity_frame(|frame| {
        let [coverage] = snapshot.sources.as_slice() else { return Err(invalid("native source coverage changed")) };
        culprit_checks::page_waits::explain(
            &[],
            Some(&SourceOutcome {
                source: coverage.source.clone(),
                state: coverage.state,
                reason: None,
                reason_message: None,
            }),
            Some(PageWaitActivityEvidence::Focused { source, original: &focus.participant, frame }),
        )
    })?;
    require_original_incident(&focus, &report)?;
    captures.verify_retained(&original).await?;
    captures.verify_retained(&detail).await?;
    crate::test_support::save_cost_receipt(
        &directory,
        "ended-page-wait-original-after-retirement",
        &serde_json::json!({
            "proof":"native-original-outbox-incident-after-workers-end",
            "original_manifest":original.manifest,"original_reference":focus.participant.parent,
            "focused_manifest":detail.manifest,"report":report,
            "native_request_outcome":result.request,"current_page_rows":snapshot.records,
            "header_or_sql_enrichment_observed":enrichment_observed,"header_lookup_eligible":result.permits_page_lookup(),
            "original_sealed_files_reverified":true,"actors_joined":completed.len(),
            "completed_fixture_inserts":completed.iter().map(|worker|worker.inserts).sum::<u64>(),
            "baseline_captures":baseline_captures,"baseline_capture_limit":MAX_BASELINES,
            "baseline_source_reader_queries":baseline_reads,"baseline_total_allowance_including_setup":baseline_total_allowance,
            "followup_source_reader_queries":detail.manifest.source_queries,
            "followup_total_allowance_including_setup":detail.manifest.request.budget.max_queries,
            "source_session_setup_allowance":(baseline_captures+1)*crate::read::ReadSession::SESSION_SETUP_QUERIES,
            "fixture_dispatch_cap_including_all_session_setup_and_database_cleanup":FIXTURE_DISPATCH_CAP,
            "combined_dispatch_cap":baseline_total_allowance+detail.manifest.request.budget.max_queries+FIXTURE_DISPATCH_CAP,
            "fixture_setup_client_ms":fixture.setup_elapsed_ms,"workload_connection_preparation_client_ms":preparation_ms,
            "fixture_work_client_ms":work_ms,"writers":WRITERS_PER_PATTERN*WRITER_APPLICATIONS.len(),
            "fixture_work_limit_ms":4000,"fixture_data_limit_mb":64,
            "native_latch_holder_proved":false,"physical_io_cause_proved":false,"page_aba_excluded":false,
            "request_id_reuse_proved":false,"native_cost_qualified":false,"live_warehouse_proved":false,
        }),
    )?;
    Ok(())
}

fn select_original(capture: &Captured) -> Result<Option<ActivityWaitFocus>> {
    let native = capture.source()?.observation.activity().ok_or_else(|| invalid("original Activity header absent"))?;
    let context = capture.activity_context()?;
    if !context.has_qualified_local_owner() || context.database_writable.is_none() || native.epoch.is_none() {
        return Err(invalid("original native member/process/role unavailable"));
    }
    if !native.coverage.complete_scope {
        return Ok(None);
    }
    for (ordinal, row) in capture.records.iter().enumerate() {
        let TelemetryRecord::Activity(original) = &row.record else {
            return Err(invalid("original Activity stream changed"));
        };
        if original.statement_identity.is_none()
            || original.scope != ActivityScopeBasis::RequestDatabase
            || !original
                .client
                .as_ref()
                .and_then(|client| client.application.as_deref())
                .is_some_and(|name| WRITER_APPLICATIONS.contains(&name))
        {
            continue;
        }
        let Some(resource) = original.wait.as_ref().and_then(|wait| {
            wait.identified_resources.iter().find(|resource| resource.kind == WaitResourceKind::MemoryPage)
        }) else {
            continue;
        };
        let focus = ActivityWaitFocus {
            resource: resource.clone(),
            participant: ActivityRequestFocus {
                partition: context.partition.clone(),
                database_writable: context.database_writable,
                parent: capture.reference(ordinal)?,
                target: context.target.clone(),
                source: capture.source()?.source.clone(),
                epoch: native.epoch.clone().unwrap(),
                original_observed_at: native.at.clone(),
                statement_identity: original.statement_identity.clone().unwrap(),
                original: original.as_ref().clone(),
            },
        };
        if !focus.is_valid() {
            return Err(invalid("sealed original did not yield a valid native page focus"));
        }
        return Ok(Some(focus));
    }
    Ok(None)
}

fn require_ended_focus(
    expected: &ActivityWaitFocus,
    result: &ActivityWaitFocusResult,
    native_rows: u32,
    rows: usize,
) -> Result<()> {
    if !result.is_valid()
        || result.focus != *expected
        || result.source != FocusSourceState::Matched
        || !matches!(result.request, PageWaitRequestState::Missing | PageWaitRequestState::Changed)
        || result.wait != PageWaitRelation::Unqualified
        || native_rows != 0
        || rows != 0
        || result.permits_page_lookup()
    {
        return Err(invalid("ended native work was replaced or promoted to a current mapped page"));
    }
    Ok(())
}
/// Restricted/oversized accounting previews cannot prove zero enrichment. The typed source still must
/// refuse lookup and publish no current page; visible forbidden work remains a hard fixture failure.
fn enrichment_observed(metrics: &ReadWorkMetrics) -> Option<bool> {
    let mut complete = !metrics.templates.is_empty();
    for template in &metrics.templates {
        match &template.statement {
            DefinitionPayload::Available { text, .. } => {
                if text.contains("sys.dm_db_page_info(") || text.contains("sys.dm_exec_sql_text(") {
                    return Some(true);
                }
            }
            DefinitionPayload::Unavailable { .. } => complete = false,
        }
    }
    complete.then_some(false)
}
fn require_original_incident(focus: &ActivityWaitFocus, report: &PageWaitReport) -> Result<()> {
    let Some(incident) = &report.recorded_incident else {
        return Err(invalid("sealed original incident was lost after retirement"));
    };
    let expected_sql = match &focus.participant.original.statement {
        DefinitionPayload::Available { text, .. } => DefinitionPayload::sql(text, focus.participant.target.engine()),
        unavailable @ DefinitionPayload::Unavailable { .. } => unavailable.clone(),
    };
    if incident.original != focus.participant.parent
        || incident.request != focus.participant.original.key
        || incident.resource != focus.resource
        || incident.observed_at != focus.participant.original_observed_at
        || incident.sql != expected_sql
        || !report.entries.is_empty()
    {
        return Err(invalid("retired incident borrowed a later request, SQL or page target"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
