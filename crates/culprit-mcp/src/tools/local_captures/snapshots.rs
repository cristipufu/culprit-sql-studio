//! Bounded historical report values and proven redacted statement previews, without private receipt internals.
use culprit_core::{
    Result,
    telemetry::{CollectionTarget, DeliveryProblem, digest},
};
use culprit_engine::{LocalSnapshotAnalysis, LocalSnapshotReport};
use serde_json::{Value, json};

const ENTRY_LIMIT: usize = 5;

pub(super) fn captured(analysis: &LocalSnapshotAnalysis) -> Result<Value> {
    let original = analysis.original();
    let read = original.read();
    let native = read.description.observation.snapshot().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let context = read.context()?;
    let mut value = json!({
        "analysis": analysis.report().kind(),
        "historical_context": true,
        "independent_shared_scope": matches!(context.target, CollectionTarget::Shared(_)),
        "original_read_bounds": {"from":context.observed_from,"until":context.observed_until},
        "native_rows":native.records,"observed_rows":read.observations.len(),"read_complete":read.read_complete,
        "native_sources":native.sources.iter().map(|source|json!({
            "state":source.state,"rows":source.records,"complete_scope":source.complete_scope,
            "stopped":source.stopped,"error_code":source.error_code,
        })).collect::<Vec<_>>(),
        "collected_from":original.collected_from(),"collected_until":original.collected_until(),
        "local_verification_bytes":original.bytes_read(),
    });
    let name = serde_json::to_value(analysis.report().kind()).map_err(|_| DeliveryProblem::Integrity.error())?;
    let name = name.as_str().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    value[name] = report_value(analysis)?;
    Ok(value)
}

fn report_value(analysis: &LocalSnapshotAnalysis) -> Result<Value> {
    Ok(match analysis.report() {
        LocalSnapshotReport::PerformanceCounters(report) => performance_counters(report),
        LocalSnapshotReport::DatabaseProperties(report) => database_properties(report),
        LocalSnapshotReport::Compilation(report) => compilation(analysis, report),
        LocalSnapshotReport::ConfiguredLogRates(report) => configured_log_rates(report),
        LocalSnapshotReport::ConnectionOutcomes(report) => connection_outcomes(analysis, report),
        LocalSnapshotReport::Jobs(report) => jobs(analysis, report),
        LocalSnapshotReport::RequestMemory(report) => request_memory(analysis, report),
        LocalSnapshotReport::TaskSpace(report) => task_space(analysis, report),
        LocalSnapshotReport::CurrentRequestWork(report) => current_request_work(analysis, report),
        LocalSnapshotReport::StatementWork(report) => statement_work(analysis, report),
        LocalSnapshotReport::MaintenanceProgress(report) => maintenance_progress(analysis, report),
        LocalSnapshotReport::CompletedSpills(report) => return completed_spills(analysis, report),
        LocalSnapshotReport::Storage(report) => storage(analysis, report),
    })
}

fn performance_counters(report: &culprit_core::PerformanceCounterReport) -> Value {
    crate::waits_view::performance_counters::value(report)
}

fn database_properties(report: &culprit_core::DatabasePropertiesReport) -> Value {
    json!({
        "title":report.title,"summary":report.summary,"source_state":report.source_state,
        "read_complete":report.read_complete,"observed_at":report.observed_at,"limitations":report.limitations,
        "fields":report.fields.iter().take(ENTRY_LIMIT).map(|field|json!({
            "key":field.key,"label":field.label,"value":field.value,
            "native_code":field.native_code,"native_description":field.native_description,
        })).collect::<Vec<_>>(),"fields_available":report.fields.len(),
        "fields_presented":report.fields.len().min(ENTRY_LIMIT),"fields_omitted":report.fields.len().saturating_sub(ENTRY_LIMIT),
        "fields_limited":report.fields.len()>ENTRY_LIMIT,
    })
}

fn compilation(analysis: &LocalSnapshotAnalysis, report: &culprit_core::CompilationReport) -> Value {
    let source = &analysis.original().read().description;
    let namespace = culprit_core::telemetry::query_store_database_namespace(&source.target, &source.partition);
    summary(
        &report.title,
        &report.summary,
        report.source_state,
        &report.source_reason,
        &report.limitations,
        report.entries.len(),
        report
            .entries
            .iter()
            .take(ENTRY_LIMIT)
            .map(|entry| {
                json!({
                    "reference":reference(analysis,&entry.observation_key),
                    "historical_native_identity": {
                        "database_namespace":namespace,"query_id":entry.identity.query_id,
                        "context_settings_id":entry.identity.qualifiers.get("context_settings_id"),
                        "object_id":entry.identity.qualifiers.get("object_id"),
                        "query_hash":entry.identity.qualifiers.get("query_hash"),
                        "initial_compile_at":entry.identity.qualifiers.get("initial_compile_at"),
                    },
                    "statement":entry.statement.as_deref().map(statement),
                    "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,
                    "investigation":entry.investigation,"verify":entry.verify,
                })
            })
            .collect(),
    )
}

fn statement(statement: &culprit_core::query_store_statement_report::QueryStoreStatementExplanation) -> Value {
    json!({
        "binding":statement.binding,"origin":statement.origin,"monitoring_template_id":statement.monitoring_template_id,
        "preview":statement.preview,"preview_truncated":statement.preview_truncated,
        "unavailable":statement.unavailable,"unavailable_explanation":statement.unavailable_explanation,
        "explanation":statement.explanation,
    })
}

fn configured_log_rates(report: &culprit_core::ConfiguredLogRateReport) -> Value {
    summary(
        &report.title,
        &report.summary,
        Some(report.source_state),
        &report.source_reason,
        &report.limitations,
        report.entries.len(),
        report
            .entries
            .iter()
            .take(ENTRY_LIMIT)
            .map(|entry| {
                json!({
                    "scope":entry.scope,"title":entry.title,"explanation":entry.explanation,"metric":entry.metric,
                })
            })
            .collect(),
    )
}

fn connection_outcomes(analysis: &LocalSnapshotAnalysis, report: &culprit_core::ConnectionOutcomeReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "reference":reference(analysis,&entry.reference.observation_key),"scope":entry.scope,
                "title":entry.title,"explanation":entry.explanation,
                "metrics":entry.metrics.iter().map(|metric|json!({
                    "kind":metric.kind,"label":metric.label,"availability":metric.availability,
                    "value":metric.display_value,"unit":metric.unit,"nature":metric.nature,
                    "reset":metric.reset,"rate":metric.rate,"explanation":metric.explanation,
                })).collect::<Vec<_>>(),"investigation":entry.investigation,"verify":entry.verify,
            })
        })
        .collect();
    let mut value =
        summary(&report.title, &report.summary, None, &None, &report.limitations, report.entries.len(), entries);
    value["mapped_rows"] = json!(report.mapped_rows);
    value["entries_limited"] = json!(report.entries_limited || report.entries.len() > ENTRY_LIMIT);

    value
}

fn jobs(analysis: &LocalSnapshotAnalysis, report: &culprit_core::JobsReport) -> Value {
    let mut value = summary(
        &report.title,
        &report.summary,
        None,
        &None,
        &report.limitations,
        report.entries.len(),
        report
            .entries
            .iter()
            .take(ENTRY_LIMIT)
            .map(|entry| {
                json!({
                    "reference":reference(analysis,&entry.observation_key),"job_id":entry.job_id,"kind":entry.kind,
                    "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,
                    "investigation":entry.investigation,"verify":entry.verify,
                })
            })
            .collect(),
    );
    value["entries_limited"] = json!(report.entries_limited || report.entries.len() > ENTRY_LIMIT);

    value
}

fn request_memory(analysis: &LocalSnapshotAnalysis, report: &culprit_core::MemoryParticipantsReport) -> Value {
    summary(&report.title,&report.summary,report.source_state,&report.source_reason,&report.limitations,report.entries.len(),
                report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                    "reference":reference(analysis,&entry.observation_key),"title":entry.title,"explanation":entry.explanation,
                    "investigation":entry.investigation,"verify":entry.verify,
                })).collect())
}

fn task_space(analysis: &LocalSnapshotAnalysis, report: &culprit_core::TaskSpaceReport) -> Value {
    summary(&report.title,&report.summary,report.source_state,&report.source_reason,&report.limitations,report.entries.len(),
                report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                    "reference":reference(analysis,&entry.observation_key),"task":entry.task,"activity":entry.activity,
                    "title":entry.title,"explanation":entry.explanation,"investigation":entry.investigation,"verify":entry.verify,
                })).collect())
}

fn current_request_work(analysis: &LocalSnapshotAnalysis, report: &culprit_core::CurrentRequestWorkReport) -> Value {
    let mut value = summary(&report.title,&report.summary,Some(report.source_state),&report.source_reason,&report.limitations,report.entries.len(),
                report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                    "reference":reference(analysis,&entry.observation_key),"actor":entry.actor,"observed_at":entry.observed_at,"scope":entry.scope,
                    "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,
                    "investigation":entry.investigation,"verify":entry.verify,
                })).collect());
    value["binding"] = json!(report.binding);
    value["membership"] = json!(report.membership);

    value
}

fn statement_work(analysis: &LocalSnapshotAnalysis, report: &culprit_core::StatementWorkReport) -> Value {
    summary(
        &report.title,
        &report.summary,
        report.source_state,
        &report.source_reason,
        &report.limitations,
        report.entries.len(),
        report
            .entries
            .iter()
            .take(ENTRY_LIMIT)
            .map(|entry| {
                json!({
                    "reference":reference(analysis,&entry.observation_key),"query_id":entry.identity.query_id,
                    "scope":entry.scope,"observed_at":entry.observed_at,"reset":entry.reset,
                    "work":entry.work,"outcomes":entry.outcomes,"outcome_metrics":entry.outcome_metrics,
                })
            })
            .collect(),
    )
}

fn maintenance_progress(analysis: &LocalSnapshotAnalysis, report: &culprit_core::MaintenanceProgressReport) -> Value {
    let mut value = summary(&report.title,&report.summary,Some(report.source_state),&report.source_reason,&report.limitations,report.entries.len(),
                report.entries.iter().take(ENTRY_LIMIT).map(|entry|json!({
                    "reference":reference(analysis,&entry.observation_key),"actor":entry.actor,"command":entry.command,"phase":entry.phase,
                    "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,
                    "investigation":entry.investigation,"verify":entry.verify,
                })).collect());
    value["binding"] = json!(report.binding);
    value["membership"] = json!(report.membership);

    value
}

fn completed_spills(analysis: &LocalSnapshotAnalysis, report: &culprit_core::CompletedSpillReport) -> Result<Value> {
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            Ok(json!({
                "reference":reference(analysis, &entry.identity.key()?),
                "origin":entry.origin,"monitoring_template_id":entry.monitoring_template_id,
                "state":entry.state,"statement_binding":entry.statement_binding,
                "statement_unavailable":entry.statement_unavailable,
                "title":entry.title,"explanation":entry.explanation,"metrics":entry.metrics,
                "investigation":entry.investigation,"verify":entry.verify,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(summary(&report.title, &report.summary, None, &None, &report.limitations, report.entries.len(), entries))
}

fn storage(analysis: &LocalSnapshotAnalysis, report: &culprit_core::StorageReport) -> Value {
    let mut value = summary(
        &report.title,
        &report.summary,
        Some(report.source_state),
        &None,
        &report.limitations,
        report.entries.len(),
        report
            .entries
            .iter()
            .take(ENTRY_LIMIT)
            .map(|entry| {
                json!({
                    "reference":reference(analysis,&entry.observation_key),"scope":entry.scope,
                    "title":entry.title,"explanation":entry.explanation,"fields":entry.fields,
                })
            })
            .collect(),
    );
    value["backend_presented_rows"] = json!(report.presented_rows);
    value["entries_limited"] = json!(report.entries_limited || report.entries.len() > ENTRY_LIMIT);

    value
}

fn summary(
    title: &str,
    text: &str,
    state: Option<culprit_core::SourceState>,
    reason: &Option<String>,
    limitations: &[String],
    available: usize,
    entries: Vec<Value>,
) -> Value {
    json!({"title":title,"summary":text,"source_state":state,"source_reason":reason,"limitations":limitations,
        "entries_available":available,"entries_presented":entries.len(),"entries_omitted":available.saturating_sub(entries.len()),"entries_limited":available>ENTRY_LIMIT,"entries":entries})
}
fn reference(analysis: &LocalSnapshotAnalysis, key: &str) -> String {
    let capture = &analysis.original().read().origin.capture;
    digest(
        format!(
            "local-snapshot-entry:{}:{}:{}:{key}",
            capture.manifest_sha256, capture.source_scope_id, capture.capture.observation_id
        )
        .as_bytes(),
    )
}
