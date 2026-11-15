//! One native Activity frame establishes participation and session blocking direction, not lock acquisition by SQL.
use culprit_core::{
    Error, MaintenanceClassification, MaintenanceDirection, MaintenanceKind, MaintenanceParticipation,
    MaintenanceReference, MaintenanceRelationIssue, MaintenanceReport, Metric, Result, SourceState,
    classify_maintenance, format, key,
    maintenance::{MAX_MAINTENANCE_ENTRIES, MAX_MAINTENANCE_OBSERVATIONS, MAX_MAINTENANCE_REFERENCES_PER_ENTRY},
    t,
    telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

pub struct MaintenanceActivityRow<'a> {
    pub observation: &'a ActivityObservation,
    pub reference: Option<&'a DiagnosticEvidenceRef>,
}

pub fn explain(
    source: &CollectionDescription,
    rows: &[MaintenanceActivityRow<'_>],
    read_complete: bool,
) -> Result<MaintenanceReport> {
    let native = validate(source, rows, read_complete)?;
    let qualified = source.is_partition_qualified() && native.epoch.is_some();
    let sessions = session_rows(rows);
    let dependents = blocking_dependents(rows, &sessions, qualified);
    let mut maintenance = Vec::new();
    let mut unclassified_requests = 0u32;
    for row in rows {
        match classify_maintenance(source.target.engine(), row.observation) {
            MaintenanceClassification::Maintenance { kind } => maintenance.push((row, kind)),
            MaintenanceClassification::Unresolved { .. } if row.observation.key.request.is_some() => {
                unclassified_requests += 1
            }
            _ => {}
        }
    }
    let maintenance_observations = maintenance.len() as u32;
    let entries = maintenance
        .into_iter()
        .take(MAX_MAINTENANCE_ENTRIES)
        .map(|(row, kind)| render(row, kind, qualified, &sessions, &dependents))
        .collect();
    let source_state = if !read_complete && matches!(native.coverage.state, SourceState::Collected | SourceState::Empty)
    {
        SourceState::Partial
    } else {
        native.coverage.state
    };
    let mut limitations = vec![
        t!("view.maintenance.limit.session"),
        t!("view.maintenance.limit.population"),
        t!("view.maintenance.limit.resources"),
        t!("view.maintenance.limit.history"),
    ];
    if !qualified {
        limitations.push(t!("view.maintenance.limit.scope"));
    }
    if !read_complete {
        limitations.push(t!("view.maintenance.limit.read"));
    }
    if native.coverage.state == SourceState::Partial {
        limitations.push(t!("view.maintenance.limit.partial"));
    }
    if matches!(native.coverage.state, SourceState::Unavailable | SourceState::NotApplicable) {
        limitations.push(t!("view.maintenance.limit.unavailable"));
    }
    if unclassified_requests > 0 {
        limitations.push(t!(
            "view.maintenance.limit.classification",
            count = format::digits(unclassified_requests.to_string())
        ));
    }
    if maintenance_observations as usize > MAX_MAINTENANCE_ENTRIES {
        limitations
            .push(t!("view.maintenance.limit.entries", count = format::digits(MAX_MAINTENANCE_ENTRIES.to_string())));
    }
    Ok(MaintenanceReport {
        source: source.source.clone(),
        context: read_context(source, native),
        native_epoch: native.epoch.clone(),
        population: native.population,
        source_state,
        source_reason: native.coverage.stopped.map(|stop| stop.error().message),
        read_complete,
        observed_rows: rows.len() as u32,
        native_rows: native.native_rows,
        maintenance_observations,
        unclassified_requests,
        entries,
        title: t!("view.maintenance.title"),
        summary: t!("view.maintenance.summary", count = format::digits(maintenance_observations.to_string())),
        limitations,
    })
}

fn validate<'a>(
    source: &'a CollectionDescription,
    rows: &[MaintenanceActivityRow<'_>],
    read_complete: bool,
) -> Result<&'a ActivitySnapshotDescription> {
    let SourceObservation::Activity(native) = &source.observation else {
        return Err(invalid());
    };
    if !source.target.is_valid()
        || !source.partition_is_consistent()
        || !native.is_valid()
        || !read_context(source, native).is_valid()
        || native.coverage.source != source.source
        || rows.len() > MAX_MAINTENANCE_OBSERVATIONS
        || rows.len() > native.observations as usize
        || read_complete && rows.len() != native.observations as usize
    {
        return Err(invalid());
    }
    ActivityEvidence::validate_references(rows.iter().map(|row| row.observation))?;
    let scope = source.captured_range()?.scope_id;
    let mut capture = None;
    let mut referenced = 0;
    let mut positions = BTreeSet::new();
    for row in rows {
        if source
            .target
            .database()
            .is_some_and(|database| !row.observation.database_context_is_consistent(&database.database))
        {
            return Err(invalid());
        }
        if let Some(reference) = row.reference {
            if !reference.is_valid()
                || reference.source_scope_id != scope
                || reference.row_ordinal >= native.observations
                || capture.as_ref().is_some_and(|expected| *expected != reference.capture_ref())
                || !positions.insert((&reference.batch_id, reference.row_ordinal))
            {
                return Err(invalid());
            }
            let exported =
                ExportedRecord::new(source, &reference.capture, TelemetryRecord::from(row.observation.clone()))?;
            if DiagnosticEvidenceRef::from_export(source, &reference.manifest_sha256, &exported, reference.row_ordinal)?
                != *reference
            {
                return Err(invalid());
            }
            capture = Some(reference.capture_ref());
            referenced += 1;
        }
    }
    if referenced != 0 && referenced != rows.len() {
        return Err(invalid());
    }
    Ok(native)
}

fn read_context(source: &CollectionDescription, native: &ActivitySnapshotDescription) -> NativeReadContext {
    NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        database_writable: native.database_writable,
    }
}

fn session_rows<'a>(
    rows: &'a [MaintenanceActivityRow<'a>],
) -> BTreeMap<&'a NativeActivityIdentity, Vec<&'a MaintenanceActivityRow<'a>>> {
    let mut sessions = BTreeMap::<_, Vec<_>>::new();
    for row in rows.iter().filter(|row| row.observation.key.request.is_some()) {
        sessions.entry(&row.observation.key.session).or_default().push(row);
    }
    sessions
}

#[derive(Default)]
struct Dependents<'a> {
    total: u32,
    retained: Vec<&'a MaintenanceActivityRow<'a>>,
}

fn blocking_dependents<'a>(
    rows: &'a [MaintenanceActivityRow<'a>],
    sessions: &BTreeMap<&NativeActivityIdentity, Vec<&MaintenanceActivityRow<'_>>>,
    qualified: bool,
) -> BTreeMap<NativeActivityIdentity, Dependents<'a>> {
    let mut dependents = BTreeMap::<_, Dependents<'_>>::new();
    if !qualified {
        return dependents;
    }
    for row in rows.iter().filter(|row| is_lock_wait(row.observation)) {
        if row.observation.blocking_status != ActivityBlockingStatus::Known {
            continue;
        }
        let owners = row
            .observation
            .wait
            .iter()
            .flat_map(|wait| &wait.owners)
            .filter_map(ActivityWaitOwner::session)
            .collect::<BTreeSet<_>>();
        for owner in owners {
            if owner == row.observation.key.session || !sessions.contains_key(&owner) {
                continue;
            }
            let entry = dependents.entry(owner).or_default();
            entry.total += 1;
            if entry.retained.len() < MAX_MAINTENANCE_REFERENCES_PER_ENTRY {
                entry.retained.push(row);
            }
        }
    }
    dependents
}

fn is_lock_wait(row: &ActivityObservation) -> bool {
    row.key.request.is_some() && row.waiting_on_lock == Some(true)
}

fn reference(row: &MaintenanceActivityRow<'_>) -> MaintenanceReference {
    MaintenanceReference {
        activity: row.observation.key.clone(),
        statement: row.observation.statement_identity.clone(),
        evidence: row.reference.cloned(),
    }
}

fn render(
    row: &MaintenanceActivityRow<'_>,
    kind: MaintenanceKind,
    qualified: bool,
    sessions: &BTreeMap<&NativeActivityIdentity, Vec<&MaintenanceActivityRow<'_>>>,
    dependents: &BTreeMap<NativeActivityIdentity, Dependents<'_>>,
) -> MaintenanceParticipation {
    let observation = row.observation;
    let affected = dependents.get(&observation.key.session);
    let affected_count = affected.map_or(0, |entry| entry.total);
    let waiting = is_lock_wait(observation);
    let direction = match (affected_count > 0, waiting) {
        (true, true) => MaintenanceDirection::Both,
        (true, false) => MaintenanceDirection::BlockingSession,
        (false, true) => MaintenanceDirection::Waiting,
        (false, false) => MaintenanceDirection::Unlinked,
    };
    let competing = sessions
        .get(&observation.key.session)
        .into_iter()
        .flatten()
        .filter(|other| other.observation.key != observation.key)
        .collect::<Vec<_>>();
    let mut issues = Vec::new();
    if !qualified {
        issues.push(MaintenanceRelationIssue::NativeScopeUnqualified);
    }
    if affected_count > 0 {
        issues.push(MaintenanceRelationIssue::SessionOwnershipOnly);
    }
    if !competing.is_empty() {
        issues.push(MaintenanceRelationIssue::MultipleOwnerRequests);
    }
    let waiting_on =
        if waiting { observation.wait.as_ref().map(|wait| wait.owners.clone()).unwrap_or_default() } else { vec![] };
    if waiting && !waiting_on.iter().any(ActivityWaitOwner::is_blocker) {
        issues.push(MaintenanceRelationIssue::WaitOwnerUnavailable);
    }
    if affected_count as usize > MAX_MAINTENANCE_REFERENCES_PER_ENTRY
        || competing.len() > MAX_MAINTENANCE_REFERENCES_PER_ENTRY
    {
        issues.push(MaintenanceRelationIssue::ReferencesLimited);
    }
    let explanation = match direction {
        MaintenanceDirection::BlockingSession => {
            t!("view.maintenance.entry.blocking", count = format::digits(affected_count.to_string()))
        }
        MaintenanceDirection::Waiting => t!("view.maintenance.entry.waiting"),
        MaintenanceDirection::Both => {
            t!("view.maintenance.entry.both", count = format::digits(affected_count.to_string()))
        }
        MaintenanceDirection::Unlinked => t!("view.maintenance.entry.unlinked"),
    };
    let mut details = vec![explanation];
    if !competing.is_empty() {
        details.push(t!("view.maintenance.entry.competing", count = format::digits(competing.len().to_string())));
    }
    if issues.contains(&MaintenanceRelationIssue::WaitOwnerUnavailable) {
        details.push(t!("view.maintenance.entry.owner_unknown"));
    }
    MaintenanceParticipation {
        reference: reference(row),
        kind,
        direction,
        affected: affected.into_iter().flat_map(|entry| &entry.retained).map(|row| reference(row)).collect(),
        affected_count,
        waiting_on,
        competing_requests: competing
            .iter()
            .take(MAX_MAINTENANCE_REFERENCES_PER_ENTRY)
            .map(|row| reference(row))
            .collect(),
        competing_request_count: competing.len() as u32,
        issues,
        title: t!(
            "view.maintenance.entry.title",
            kind = kind_label(kind),
            session = observation.key.session.id.as_str()
        ),
        explanation: details.join(" "),
        metrics: metrics(observation),
        investigation: match direction {
            MaintenanceDirection::BlockingSession | MaintenanceDirection::Both => {
                t!("view.maintenance.investigate.blocking")
            }
            MaintenanceDirection::Waiting => t!("view.maintenance.investigate.waiting"),
            MaintenanceDirection::Unlinked => t!("view.maintenance.investigate.unlinked"),
        },
        verify: t!("view.maintenance.verify"),
    }
}

fn kind_label(kind: MaintenanceKind) -> String {
    match kind {
        MaintenanceKind::IndexCreate => t!("view.maintenance.kind.index_create"),
        MaintenanceKind::IndexReplace => t!("view.maintenance.kind.index_replace"),
        MaintenanceKind::IndexRebuild => t!("view.maintenance.kind.index_rebuild"),
        MaintenanceKind::IndexReorganize => t!("view.maintenance.kind.index_reorganize"),
        MaintenanceKind::IndexReindex => t!("view.maintenance.kind.index_reindex"),
        MaintenanceKind::Cluster => t!("view.maintenance.kind.cluster"),
        MaintenanceKind::Vacuum => t!("view.maintenance.kind.vacuum"),
        MaintenanceKind::Statistics => t!("view.maintenance.kind.statistics"),
        MaintenanceKind::TableOptimize => t!("view.maintenance.kind.table_optimize"),
        MaintenanceKind::TableRepair => t!("view.maintenance.kind.table_repair"),
    }
}

fn metrics(row: &ActivityObservation) -> Vec<Metric> {
    let mut metrics = Vec::new();
    if let Some(elapsed) = &row.elapsed_ms {
        metrics.push(Metric::said_text(
            key!("view.maintenance.metric.elapsed"),
            t!("view.maintenance.metric.milliseconds", count = format::digits(elapsed.clone())),
        ));
    }
    if let Some(counters) = &row.request_counters {
        if let Some(cpu) = &counters.cpu_ms {
            metrics.push(Metric::said_text(
                key!("view.maintenance.metric.cpu"),
                t!("view.maintenance.metric.milliseconds", count = format::digits(cpu.clone())),
            ));
        }
        for (label, value) in [
            (key!("view.maintenance.metric.logical_reads"), &counters.logical_reads),
            (key!("view.maintenance.metric.physical_reads"), &counters.physical_reads),
            (key!("view.maintenance.metric.writes"), &counters.writes),
        ] {
            if let Some(value) = value {
                metrics.push(Metric::said_text(label, format::digits(value.clone())));
            }
        }
    }
    metrics
}

fn invalid() -> Error {
    Error::invalid(t!("collection.invalid_source"))
}

#[cfg(test)]
mod tests;
