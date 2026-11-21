//! One pure interpretation of original native Galera configuration and status; no database health grading.
use culprit_core::{
    GaleraOrigin, GaleraReport, MeasurementScope, ReplicationEntry, ReplicationField, ReplicationFieldGroup,
    ReplicationReference, Result, SourceOutcome, SourceState, format, t, telemetry::*,
};

pub struct GaleraEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<GaleraReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(GaleraEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: GaleraEvidence<'_>) -> Result<GaleraReport> {
    let context = galera_context(evidence.source)?;
    let SourceObservation::Galera { snapshot, metadata } = &evidence.source.observation else { return Err(invalid()) };
    if evidence.observations.len() > snapshot.records as usize
        || evidence.read_complete && evidence.observations.len() != snapshot.records as usize
    {
        return Err(invalid());
    }
    let mut entries = Vec::new();
    for row in evidence.observations {
        let OperationalFact::MariadbGalera(point) = &row.fact else { return Err(invalid()) };
        if !row.is_valid() || !point.fits_source(evidence.source) {
            return Err(invalid());
        }
        entries.push(entry(row, point));
    }
    let coverage = &snapshot.sources[0];
    let reason = if let Some(stop) = coverage.stopped {
        Some(stop.error().message)
    } else if coverage.error_code == Some(culprit_core::ErrorCode::PermissionDenied) {
        Some(t!("view.galera.denied"))
    } else if coverage.error_code == Some(culprit_core::ErrorCode::Unsupported) {
        Some(t!("view.galera.unsupported"))
    } else if coverage.error_code.is_some() {
        Some(t!("view.galera.unavailable"))
    } else if !evidence.read_complete || coverage.state == SourceState::Partial {
        Some(t!("view.galera.partial"))
    } else {
        None
    };
    let source = SourceOutcome {
        source: coverage.source.clone(),
        state: if !evidence.read_complete && matches!(coverage.state, SourceState::Collected | SourceState::Empty) {
            SourceState::Partial
        } else {
            coverage.state
        },
        reason,
        reason_message: None,
    };
    let mut limitations = limitations();
    if !evidence.read_complete || coverage.state == SourceState::Partial {
        limitations.push(t!("view.galera.partial"));
    }
    Ok(GaleraReport {
        original: Some(GaleraOrigin { context, metadata: metadata.as_ref().clone() }),
        sources: vec![source],
        read_complete: evidence.read_complete,
        native_rows: Some(snapshot.records),
        observed_rows: evidence.observations.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited: false,
        title: t!("view.galera.title"),
        summary: if metadata.enabled == Some(false) {
            t!("view.galera.disabled")
        } else if entries.is_empty() {
            t!("view.galera.no_status")
        } else {
            t!("view.galera.summary")
        },
        limitations,
        entries,
    })
}
fn entry(row: &OperationalObservation, point: &MariadbGaleraPoint) -> ReplicationEntry {
    ReplicationEntry {
        reference: ReplicationReference { source: row.source.clone(), observation_key: row.key.clone() },
        scope: MeasurementScope::Server,
        conditions: Vec::new(),
        title: t!("view.galera.node_title"),
        explanation: t!("view.galera.node_context"),
        fields: vec![
            text(point, "wsrep_ready", culprit_core::key!("view.galera.ready"), point.ready.map(boolean).as_deref()),
            text(
                point,
                "wsrep_connected",
                culprit_core::key!("view.galera.connected"),
                point.connected.map(boolean).as_deref(),
            ),
            text(
                point,
                "wsrep_cluster_status",
                culprit_core::key!("view.galera.component"),
                point.component_status.as_deref(),
            ),
            quantity(point, "wsrep_local_state", culprit_core::key!("view.galera.state"), point.local_state.as_deref()),
            text(
                point,
                "wsrep_local_state_comment",
                culprit_core::key!("view.galera.state_comment"),
                point.local_state_comment.as_deref(),
            ),
            quantity(
                point,
                "wsrep_cluster_size",
                culprit_core::key!("view.galera.cluster_size"),
                point.cluster_size.as_deref(),
            ),
            text(
                point,
                "wsrep_cluster_state_uuid",
                culprit_core::key!("view.galera.cluster_uuid"),
                point.cluster_state_uuid.as_deref(),
            ),
            text(
                point,
                "wsrep_local_state_uuid",
                culprit_core::key!("view.galera.local_uuid"),
                point.local_state_uuid.as_deref(),
            ),
            text(point, "wsrep_gcomm_uuid", culprit_core::key!("view.galera.node_uuid"), point.node_uuid.as_deref()),
        ],
        groups: vec![
            ReplicationFieldGroup {
                title: t!("view.galera.queues"),
                fields: vec![
                    quantity(
                        point,
                        "wsrep_local_recv_queue",
                        culprit_core::key!("view.galera.receive_queue"),
                        point.receive_queue.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_local_send_queue",
                        culprit_core::key!("view.galera.send_queue"),
                        point.send_queue.as_deref(),
                    ),
                ],
            },
            ReplicationFieldGroup {
                title: t!("view.galera.totals"),
                fields: vec![
                    quantity(
                        point,
                        "wsrep_local_cert_failures",
                        culprit_core::key!("view.galera.certification_failures"),
                        point.certification_failures.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_local_bf_aborts",
                        culprit_core::key!("view.galera.applier_aborts"),
                        point.applier_aborts.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_local_replays",
                        culprit_core::key!("view.galera.replays"),
                        point.replays.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_local_commits",
                        culprit_core::key!("view.galera.commits"),
                        point.local_commits.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_flow_control_paused_ns",
                        culprit_core::key!("view.galera.paused_ns"),
                        point.paused_nanoseconds.as_deref(),
                    ),
                    quantity(
                        point,
                        "wsrep_flow_control_paused",
                        culprit_core::key!("view.galera.paused_fraction"),
                        point.paused_fraction.as_deref(),
                    ),
                ],
            },
        ],
        session: None,
        unresolved_session_id: None,
        investigation: t!("view.galera.investigation"),
        verify: t!("view.galera.verify"),
    }
}
fn text(point: &MariadbGaleraPoint, native: &str, label: &'static str, value: Option<&str>) -> ReplicationField {
    ReplicationField {
        key: native.into(),
        label: culprit_core::i18n::t(label, &[]),
        value: if point.has(native) { value.map(str::to_owned) } else { Some(t!("view.galera.variable_absent")) },
    }
}
fn quantity(point: &MariadbGaleraPoint, native: &str, label: &'static str, value: Option<&str>) -> ReplicationField {
    let mut field = text(point, native, label, value);
    if let Some(value) = value {
        field.value = Some(format::digits(value.to_owned()));
    }
    field
}
fn boolean(value: bool) -> String {
    if value { t!("view.galera.on") } else { t!("view.galera.off") }
}
fn limitations() -> Vec<String> {
    vec![t!("view.galera.scope"), t!("view.galera.reset"), t!("view.galera.non_atomic"), t!("view.galera.unknown")]
}
pub fn explain_recorded(recorded: &RecordedOperationalRead) -> Result<GaleraReport> {
    match recorded {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> GaleraReport {
    let reason_text = reason.render();
    let mut limitations = limitations();
    limitations.push(reason_text.clone());
    GaleraReport {
        original: None,
        sources: vec![SourceOutcome {
            source: MARIADB_GALERA_SOURCE.into(),
            state: SourceState::Unavailable,
            reason: Some(reason_text),
            reason_message: Some(reason.clone()),
        }],
        read_complete: false,
        native_rows: None,
        observed_rows: 0,
        presented_rows: 0,
        entries_limited: false,
        title: t!("view.galera.title"),
        summary: t!("view.galera.no_status"),
        limitations,
        entries: Vec::new(),
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
