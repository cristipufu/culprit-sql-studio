//! A verified historical query capture can feed the existing pure checks without a database provider or credentials.
use culprit_collector::analytical::evidence::EvidenceCapture;
use culprit_core::{
    CollectorNote, Environment, MissingCapability, Result, SNAPSHOT_FORMAT, ServerInfo, Snapshot, SourceOutcome,
    SourceState, Workload,
    collected_analysis::*,
    serialization::{JsonSizeError, json_size},
    source_id,
    telemetry::DeliveryProblem,
};

mod groups;
mod populations;
mod validation;
mod waits;

const MAX_RETAINED_ANALYSIS_BYTES: usize = 256 * 1024 * 1024;

/// Analyze one explicitly selected capture. This cannot merge overlapping captures, source epochs or databases.
pub fn analyze(
    capture: &EvidenceCapture,
    population: Option<&culprit_core::telemetry::NativeIdentity>,
) -> Result<CollectedQueryAnalysis> {
    analyze_with_limit(capture, population, MAX_RETAINED_ANALYSIS_BYTES)
}

fn analyze_with_limit(
    capture: &EvidenceCapture,
    population: Option<&culprit_core::telemetry::NativeIdentity>,
    limit: usize,
) -> Result<CollectedQueryAnalysis> {
    let database = validation::capture(capture)?;
    let selected = populations::PopulationIndex::new(capture, population)?;
    let definitions = validation::Definitions::new(capture)?;
    let mut analysis = CollectedQueryAnalysis {
        context: crate::collected_context::from_capture(capture),
        requested_population: population.cloned(),
        analyses: Vec::new(),
    };
    // The empty array already accounts for both brackets. Only each item and separators are added below; the
    // original context, including every retained native payload, is charged exactly once before derivation.
    let mut retained = json_size(&analysis, limit).map_err(output_size_error)?;
    for population in selected.selected() {
        let result =
            analyze_population(capture, &definitions, &database, population.clone(), selected.rows(population)?)?;
        let comma = usize::from(!analysis.analyses.is_empty());
        let remaining = limit
            .checked_sub(retained)
            .and_then(|remaining| remaining.checked_sub(comma))
            .ok_or_else(|| output_size_error(JsonSizeError::LimitExceeded))?;
        let bytes = json_size(&result, remaining).map_err(output_size_error)?;
        retained += comma + bytes;
        analysis.analyses.push(result);
    }
    Ok(analysis)
}

fn output_size_error(error: JsonSizeError) -> culprit_core::Error {
    match error {
        JsonSizeError::LimitExceeded => {
            DeliveryProblem::Capacity.error().with_hint(culprit_core::t!("engine.collected_analysis.output_limit_hint"))
        }
        JsonSizeError::Serialization => DeliveryProblem::Integrity.error(),
    }
}

fn analyze_population(
    capture: &EvidenceCapture,
    definitions: &validation::Definitions<'_>,
    database: &culprit_core::DatabaseIdentity,
    population: CollectedQueryPopulation,
    rows: &populations::NativeRows<'_>,
) -> Result<CollectedQueryAnalysisResult> {
    let mut limitations = vec![
        limit(None, CollectedAnalysisLimit::MissingCurrentServerMetadata),
        limit(None, CollectedAnalysisLimit::MissingSchema),
        limit(None, CollectedAnalysisLimit::MissingSettings),
        limit(None, CollectedAnalysisLimit::MissingResourceMeasurements),
    ];
    let (workload, query_inputs, partial) = if population == CollectedQueryPopulation::Unknown {
        limitations.push(limit(None, CollectedAnalysisLimit::ExecutionPopulationUnknown));
        (None, vec![], true)
    } else {
        let (queries, inputs, partial) = groups::project(capture, definitions, &population, rows, &mut limitations)?;
        (workload(capture, definitions, &population, rows, queries, &mut limitations)?, inputs, partial)
    };
    let mut report = culprit_checks::analyze(snapshot(capture, database.clone(), workload, partial));
    if let Some(provenance) = &mut report.provenance {
        provenance.origin = culprit_core::AnalysisOrigin::ArchivedTelemetry;
    }
    apply_executable_fix_policy(&mut report.findings, report.snapshot.workload.as_mut(), &mut limitations);
    Ok(CollectedQueryAnalysisResult {
        population,
        query_inputs,
        workload: report.snapshot.workload,
        findings: report.findings,
        assessments: report.assessments,
        locked: report.locked,
        limitations,
        provenance: report.provenance,
    })
}

fn workload(
    capture: &EvidenceCapture,
    definitions: &validation::Definitions<'_>,
    population: &CollectedQueryPopulation,
    rows: &populations::NativeRows<'_>,
    queries: Vec<culprit_core::QueryStat>,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<Workload>> {
    if queries.is_empty() {
        return Ok(None);
    }
    let history = capture.source().query_history().ok_or_else(invalid)?;
    let Some(totals) = groups::captured_totals(capture, definitions, rows)? else {
        limitations.push(limit(None, CollectedAnalysisLimit::RuntimeDenominatorUnavailable));
        return Ok(None);
    };
    let hours = validation::hours(&history.interval.from, &history.interval.until)?;
    Ok(Some(Workload {
        population: match population {
            CollectedQueryPopulation::Population { identity } => Some(identity.clone()),
            _ => None,
        },
        population_catalog: None,
        source: capture.source().source.clone(),
        kind: None,
        windowed: true,
        log: None,
        window_start: Some(history.interval.from.clone()),
        window_end: Some(history.interval.until.clone()),
        window_hours: hours,
        total_time_ms: totals.time_ms,
        total_cpu_ms: totals.cpu_ms,
        total_executions: totals.executions as f64,
        query_count: totals.queries as f64,
        queries,
        timeline: None,
        untracked_time_ms: None,
        hourly_time_ms: None,
        min_logged_ms: None,
        history_inputs: vec![],
        counter_set: None,
        source_label: None,
        source_message: None,
    }))
}

fn snapshot(
    capture: &EvidenceCapture,
    database: culprit_core::DatabaseIdentity,
    workload: Option<Workload>,
    partial: bool,
) -> Snapshot {
    let have_plans =
        workload.as_ref().is_some_and(|workload| workload.queries.iter().any(|query| query.plan.is_some()));
    let reason = culprit_core::message!("engine.collected_analysis.not_captured");
    let mut sources = source_id::ALL
        .iter()
        .map(|source| SourceOutcome {
            source: (*source).into(),
            state: SourceState::Unavailable,
            reason: Some(reason.render()),
            reason_message: Some(reason.clone()),
        })
        .collect::<Vec<_>>();
    if workload.is_some()
        && let Some(source) = sources.iter_mut().find(|source| source.source == source_id::WORKLOAD)
    {
        source.state = if partial { SourceState::Partial } else { SourceState::Collected };
        source.reason = partial.then(|| culprit_core::t!("engine.collected_analysis.partial_workload"));
        source.reason_message = partial.then(|| culprit_core::message!("engine.collected_analysis.partial_workload"));
    }
    if let Some(workload) = &workload
        && workload.queries.iter().any(|query| query.waits_coverage.is_some())
        && let Some(source) = sources.iter_mut().find(|source| source.source == source_id::QUERY_WAITS)
    {
        let complete = workload.queries.iter().all(|query| {
            query.waits_coverage.as_ref().is_some_and(|coverage| coverage.complete && coverage.is_valid())
        });
        source.state = if complete { SourceState::Collected } else { SourceState::Partial };
        source.reason = (!complete).then(|| culprit_core::t!("engine.collected_analysis.partial_workload"));
        source.reason_message =
            (!complete).then(|| culprit_core::message!("engine.collected_analysis.partial_workload"));
    }
    if have_plans && let Some(source) = sources.iter_mut().find(|source| source.source == source_id::RETAINED_PLANS) {
        source.state = SourceState::Partial;
        source.reason = Some(culprit_core::t!("engine.collected_analysis.partial_workload"));
        source.reason_message = Some(culprit_core::message!("engine.collected_analysis.partial_workload"));
    }
    let capabilities =
        culprit_checks::infos().into_iter().flat_map(|info| info.requires).collect::<std::collections::BTreeSet<_>>();
    let server = ServerInfo {
        engine: database.engine,
        database: database.database.clone(),
        product: String::new(),
        version: String::new(),
        edition: None,
        cloud: None,
        platform: None,
        database_size_bytes: None,
        started_at: None,
        read_only_session: false,
        capabilities: if have_plans { vec![culprit_core::Capability::QueryPlans] } else { vec![] },
        missing: capabilities
            .into_iter()
            .filter(|capability| !have_plans || *capability != culprit_core::Capability::QueryPlans)
            .map(|capability| MissingCapability::new(capability, reason.clone()))
            .collect(),
    };
    Snapshot {
        format: SNAPSHOT_FORMAT,
        id: capture.range_id().into(),
        connection: String::new(),
        // No connection is opened and no environment/session claim is exposed by CollectedQueryAnalysis.
        environment: Environment::Production,
        taken_at: capture.collected_until().into(),
        duration_ms: 0.0,
        server,
        runtime_populations: None,
        workload,
        tables: vec![],
        indexes: vec![],
        columns: vec![],
        foreign_keys: vec![],
        missing_indexes: vec![],
        statistics: vec![],
        waits: vec![],
        resources: vec![],
        settings: vec![],
        notes: vec![CollectorNote::recorded("collected-evidence", reason)],
        pressure: None,
        options: None,
        query_families: vec![],
        plan_cache: None,
        tuning: None,
        deadlocks: None,
        constraints: vec![],
        histograms: None,
        escalations_before: None,
        compilations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: Default::default(),
        sources,
        identity: Some(database),
    }
}

pub(super) fn limit(record_id: Option<String>, reason: CollectedAnalysisLimit) -> CollectedAnalysisLimitation {
    CollectedAnalysisLimitation { record_id, reason }
}

fn apply_executable_fix_policy(
    findings: &mut [culprit_core::Finding],
    workload: Option<&mut Workload>,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) {
    let prerequisites_missing = limitations.iter().any(|limitation| {
        matches!(
            limitation.reason,
            CollectedAnalysisLimit::MissingCurrentServerMetadata
                | CollectedAnalysisLimit::MissingSchema
                | CollectedAnalysisLimit::MissingSettings
        )
    });
    if !prerequisites_missing {
        return;
    }
    // This decision follows missing native prerequisites, not the age or storage location of the input.
    // Preserve the check's plain-language advice and verification while withholding unestablished executable SQL.
    for finding in findings {
        if let Some(fix) = &mut finding.fix
            && fix.sql.take().is_some()
        {
            limitations
                .push(limit(Some(finding.id.clone()), CollectedAnalysisLimit::ExecutableFixPrerequisitesUnknown));
        }
    }
    for query in workload.into_iter().flat_map(|workload| &mut workload.queries) {
        let mut changed = false;
        for plan in query
            .plan
            .iter_mut()
            .chain(query.plan_history.iter_mut().filter_map(|history| history.captured_plan.as_mut()))
        {
            changed |= crate::archived_advice::withhold_plan_sql(plan);
        }
        if changed {
            limitations.push(limit(Some(query.id.clone()), CollectedAnalysisLimit::ExecutableFixPrerequisitesUnknown));
        }
    }
}

#[cfg(test)]
#[path = "collected_evidence/advice_tests.rs"]
mod advice_tests;

#[cfg(test)]
#[path = "collected_evidence/retention_tests.rs"]
mod retention_tests;

pub(super) fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
