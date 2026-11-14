//! This proves native provider behavior only. Durable manifests, warehouse authentication and episode replay
//! are independently exercised by engine/collector tests; this fixture creates no telemetry destinations.
use super::fixture::{self, Fixture, invalid};
use culprit_core::{Result, WaitResourceKind, telemetry::*};
use std::time::{Duration, Instant};

const MAX_BASELINES: usize = 8;

struct NativeCapture {
    request: CollectionRequest,
    description: CollectionDescription,
    records: Vec<TelemetryRecord>,
    pages: u32,
    queries: u32,
    metrics: ReadWorkMetrics,
    elapsed_ms: f64,
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn a_retained_native_baseline_survives_work_ending_before_its_focused_lookup() {
    fixture::run(|fixture| {
        Box::pin(async move {
            let preparing = Instant::now();
            let work = fixture.start_workload(false, false).await?;
            let preparation_ms = preparing.elapsed().as_secs_f64() * 1000.0;
            let mut baseline_queries = 0;
            let mut baseline_ms = 0.0;
            let mut selected = None;
            for attempt in 0..MAX_BASELINES {
                if fixture.workers_finished() {
                    break;
                }
                let captured = capture(fixture, CollectionSelection::Activity).await?;
                baseline_queries += captured.queries;
                baseline_ms += captured.elapsed_ms;
            eprintln!("{}", serde_json::json!({"phase":"owned-native-baseline", "attempt":attempt,
                "rows":captured.records.len(),"native_queries":captured.queries,"observer_client_ms":captured.elapsed_ms}));
                let activity = captured
                    .description
                    .observation
                    .activity()
                    .ok_or_else(|| invalid("native activity descriptor absent"))?;
                if activity.epoch.is_none() || captured.queries != 7 {
                    return Err(invalid(
                        "owned boxed baseline did not establish the declared seven-phase native authority",
                    ));
                }
                let ordinal = captured.records.iter().position(|record| {
                    let TelemetryRecord::Activity(row) = record else { return false };
                    row.statement_identity.is_some()
                        && row
                            .client
                            .as_ref()
                            .and_then(|client| client.application.as_deref())
                            .is_some_and(|name| matches!(name, "page-fixture-write-a" | "page-fixture-write-b"))
                        && row.wait.as_ref().is_some_and(|wait| {
                            wait.identified_resources
                                .iter()
                                .any(|resource| resource.kind == WaitResourceKind::MemoryPage)
                        })
                });
                if let Some(ordinal) = ordinal {
                    let (parent, original) = retained_parent(&captured, ordinal, attempt)?;
                    let resource = original
                        .wait
                        .as_ref()
                        .unwrap()
                        .identified_resources
                        .iter()
                        .find(|resource| resource.kind == WaitResourceKind::MemoryPage)
                        .unwrap()
                        .clone();
                    selected = Some(ActivityWaitFocus {
                        parent,
                        target: captured.description.target.clone(),
                        source: captured.description.source.clone(),
                        epoch: activity.epoch.clone().unwrap(),
                        original_observed_at: activity.at.clone(),
                        statement_identity: original.statement_identity.clone().unwrap(),
                        original,
                        resource,
                    });
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let completed = fixture.finish_workload().await?;
            let work_ms = work.elapsed().as_secs_f64() * 1000.0;
            let focus = selected.ok_or_else(|| {
                invalid("finite owned workload did not produce a captured page-latch participant; no proof")
            })?;
            if !focus.is_valid() {
                return Err(invalid("original native row receipt did not yield a valid focus"));
            }
            let original = focus.original.clone();
            let parent = focus.parent.clone();
            let detail =
                capture(fixture, CollectionSelection::FocusedPageWait { focus: Box::new(focus.clone()) }).await?;
            let SourceObservation::FocusedPageWait { snapshot, result } = &detail.description.observation else {
                return Err(invalid("native followup lost its typed focused outcome"));
            };
            if !matches!(result.request, PageWaitRequestState::Missing | PageWaitRequestState::Changed)
                || snapshot.records != 0
                || !detail.records.is_empty()
                || result.focus.original != original
                || result.focus.parent != parent
                || result.focus != focus
            {
                return Err(invalid("ended native work was replaced, erased or described as a current mapped page"));
            }
            if detail.queries != 10 {
                return Err(invalid(
                    "focused ended-request source escaped its exact native context/target/finish ledger",
                ));
            }
            let mut headers = 0;
            for template in &detail.metrics.templates {
                if let DefinitionPayload::Available { text, .. } = &template.statement {
                    if text.contains("sys.dm_db_page_info(") || text.contains("sys.dm_exec_sql_text(") {
                        return Err(invalid("ended focus performed forbidden header or SQL enrichment work"));
                    }
                    headers += usize::from(text.contains("OUTER APPLY sys.dm_db_page_info"));
                } else {
                    return Err(invalid("native diagnostic template was unavailable for bounded work verification"));
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "proof":"native-baseline-retained-before-ended-focus", "writers":16,"fixture_work_limit_ms":4000,
                    "fixture_data_limit_mb":64,"setup_client_elapsed_ms":fixture.setup_elapsed_ms,
                    "workload_connection_preparation_client_ms":preparation_ms,"fixture_work_client_ms":work_ms,
                    "completed_fixture_inserts":completed.iter().map(|worker|worker.inserts).sum::<u64>(),
                    "baseline_native_queries":baseline_queries,"baseline_observer_client_ms":baseline_ms,
                    "focused_native_queries":detail.queries,"focused_observer_client_ms":detail.elapsed_ms,
                    "focused_request":result.request,"focused_page_rows":snapshot.records,"header_reads":headers,
                    "native_cpu_io_cost_qualified":false,"durability_or_warehouse_proof":false,
                    "baseline_parent_receipt":parent.key()?,"focused_read_metrics":detail.metrics,
                })
            );
            Ok(())
        })
    })
    .await;
}

async fn capture(fixture: &Fixture, selection: CollectionSelection) -> Result<NativeCapture> {
    let declaration = crate::telemetry::recipes()
        .into_iter()
        .find(|recipe| {
            recipe.selector.matches(&CollectionRequest {
                subject: CollectionSubject::Database,
                selection: selection.clone(),
                budget: CollectionBudget::default(),
            })
        })
        .ok_or_else(|| invalid("native fixture recipe was not declared"))?;
    let request = CollectionRequest { subject: CollectionSubject::Database, selection, budget: declaration.budget };
    let started = Instant::now();
    let mut source =
        crate::telemetry::source(fixture.pool.as_ref().unwrap().clone(), &request, "127.0.0.1".into(), fixture.port)?;
    let collected = async {
        if source.query_count() != 0 {
            return Err(invalid("native factory performed work before describe"));
        }
        let description = source.describe().await?;
        if !description.matches_request(&request) {
            return Err(invalid("native descriptor did not match its bounded request"));
        }
        let mut cursor = None;
        let mut records = Vec::new();
        let mut pages = 0;
        loop {
            if pages == request.budget.max_pages {
                return Err(invalid("native fixture exceeded finite page bound"));
            }
            let CollectionRead::Page { records: page, next_cursor } =
                source.read_page(request.collection().source_streams()[0], cursor.as_deref()).await?
            else {
                return Err(invalid("native fixture source stopped before complete frozen delivery"));
            };
            pages += 1;
            if records.len() + page.len() > request.budget.max_snapshot_rows as usize {
                return Err(invalid("native fixture exceeded declared row bound"));
            }
            records.extend(page);
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if source.finish().await? != SourceCompletion::Verified {
            return Err(invalid("native fixture source could not verify its frozen result"));
        }
        Ok((description, records, pages))
    }
    .await;
    let shutdown = source.close().await;
    let queries = source.query_count();
    let metrics = source.read_metrics().ok_or_else(|| invalid("native read metrics were absent"))?;
    if collected.is_err() || shutdown != SourceShutdown::Quiescent || metrics.has_unconfirmed_work() {
        eprintln!(
            "owned focused native proof failure: error={:?},shutdown={shutdown:?},metrics={}",
            collected.as_ref().err(),
            serde_json::to_string(&metrics).map_err(|_| invalid("native metrics encoding failed"))?
        );
    }
    if shutdown != SourceShutdown::Quiescent
        || metrics.has_unconfirmed_work()
        || metrics.untracked_tasks != 0
        || metrics.templates.iter().any(|task| task.failures != 0 || task.timeouts != 0)
    {
        return Err(invalid("native source did not close every diagnostic owner quiescently"));
    }
    let (description, records, pages) = collected?;
    Ok(NativeCapture {
        request,
        description,
        records,
        pages,
        queries,
        metrics,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn retained_parent(
    capture: &NativeCapture,
    ordinal: usize,
    attempt: usize,
) -> Result<(DiagnosticEvidenceRef, ActivityObservation)> {
    let identity = CaptureIdentity {
        observation_id: format!("native-baseline-{}-{attempt}", std::process::id()),
        revision: Some(CaptureRevision {
            outbox_id: "owned-native-provider-protocol-fixture".into(),
            owner_epoch: "1".into(),
            sequence: (attempt + 1).to_string(),
        }),
    };
    let exported = capture
        .records
        .iter()
        .map(|row| ExportedRecord::new(&capture.description, &identity, row.clone()))
        .collect::<Result<Vec<_>>>()?;
    let mut bytes = Vec::new();
    for row in &exported {
        serde_json::to_writer(&mut bytes, row).map_err(|_| invalid("native export encoding failed"))?;
        bytes.push(b'\n');
    }
    if bytes.len() > capture.request.budget.total_bytes as usize {
        return Err(invalid("in-memory native fixture export exceeded its finite byte bound"));
    }
    let activity =
        capture.description.observation.activity().ok_or_else(|| invalid("native baseline descriptor missing"))?;
    let manifest = CollectionManifest {
        format_version: EXPORT_FORMAT_VERSION,
        capture: identity.clone(),
        started_at: activity.observed_from.clone(),
        finished_at: Some(activity.observed_until.clone()),
        request: capture.request.clone(),
        investigation: None,
        description: Some(capture.description.clone()),
        status: CollectionStatus::Complete,
        source_queries: capture.queries,
        read_metrics: Some(capture.metrics.clone()),
        directory_synced: false,
        files: vec![CollectedFile {
            batch_id: batch_id(&identity.observation_id, TelemetryStream::ActivityObservations)?,
            stream: TelemetryStream::ActivityObservations,
            semantics: TelemetrySemantics::Sample,
            file: TelemetryStream::ActivityObservations.file_name().into(),
            status: CollectionStatus::Complete,
            error_code: None,
            rows: exported.len() as u32,
            bytes: bytes.len() as u32,
            pages: capture.pages,
            sha256: Some(digest(&bytes)),
        }],
        references: ReferenceCoverage::default(),
    };
    let manifest_sha =
        digest(&serde_json::to_vec(&manifest).map_err(|_| invalid("native in-memory manifest encoding failed"))?);
    let row = exported.get(ordinal).ok_or_else(|| invalid("native selected row absent"))?;
    let parent = DiagnosticEvidenceRef::from_export(&capture.description, &manifest_sha, row, ordinal as u32)?;
    let TelemetryRecord::Activity(original) = row.record.clone() else {
        return Err(invalid("native selected row was not activity"));
    };
    Ok((parent, *original))
}
