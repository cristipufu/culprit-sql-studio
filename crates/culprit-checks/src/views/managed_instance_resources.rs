//! One interpretation for authenticated history, saved reports and every display surface.
use culprit_core::{
    ManagedInstanceResourceField, ManagedInstanceResourceInterval, ManagedInstanceResourceReport, Result, SourceState,
    format, t, telemetry::*,
};
pub struct ManagedInstanceResourceEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [ResourceMeasurement],
    pub read_complete: bool,
}

pub fn explain(evidence: ManagedInstanceResourceEvidence<'_>) -> Result<ManagedInstanceResourceReport> {
    let history =
        validate_managed_instance_resource_rows(evidence.source, evidence.observations, evidence.read_complete)?;
    let mut report = unavailable(&culprit_core::message!("view.mi_resources.unavailable"));
    report.source_state =
        if !evidence.read_complete && matches!(history.coverage.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            history.coverage.state
        };
    report.native_source = Some(history.coverage.clone());
    report.read_complete = evidence.read_complete;
    report.from = Some(history.from.clone());
    report.until = Some(history.until.clone());
    report.observed_at = Some(history.observed_at.clone());
    report.native_rows =
        matches!(history.coverage.state, SourceState::Collected | SourceState::Partial).then_some(history.native_rows);
    report.recorded_rows = Some(history.coverage.records);
    report.observed_rows = evidence.observations.len() as u32;
    report.layout = Some(history.layout.clone());
    if !evidence.observations.is_empty() {
        report.summary = t!(
            "view.mi_resources.summary",
            n = report.observed_rows,
            count = format::digits(report.observed_rows.to_string())
        );
    }
    if !evidence.read_complete {
        report.limitations.push(t!("view.mi_resources.limit.body"));
    }
    if !history.coverage.complete_scope {
        report.limitations.push(t!("view.mi_resources.limit.partial"));
    }
    if let Some(code) = history.coverage.error_code {
        report.limitations.push(match code {
            culprit_core::ErrorCode::PermissionDenied => t!("view.mi_resources.limit.permission"),
            culprit_core::ErrorCode::Unsupported => t!("view.mi_resources.limit.unsupported"),
            _ => t!("view.mi_resources.limit.failed"),
        });
    }
    if let Some(stop) = history.coverage.stopped {
        report.limitations.push(stop.error().message);
    }
    for row in evidence.observations {
        let native = row.mssql_managed_instance.as_ref().ok_or_else(invalid)?;
        report.intervals.push(interval(native, &history.layout));
    }
    Ok(report)
}
pub fn explain_captured(read: &CapturedManagedInstanceResourceRead) -> Result<ManagedInstanceResourceReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(ManagedInstanceResourceEvidence {
        source: &read.description,
        observations: &read.observations,
        read_complete: read.read_complete,
    })
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> ManagedInstanceResourceReport {
    ManagedInstanceResourceReport {
        title: t!("view.mi_resources.title"),
        summary: reason.render(),
        source_state: SourceState::Unavailable,
        native_source: None,
        read_complete: false,
        from: None,
        until: None,
        observed_at: None,
        native_rows: None,
        recorded_rows: None,
        observed_rows: 0,
        layout: None,
        intervals: vec![],
        limitations: vec![
            t!("view.mi_resources.limit.scope"),
            t!("view.mi_resources.limit.publication"),
            t!("view.mi_resources.limit.meaning"),
            t!("view.mi_resources.limit.clock"),
        ],
        investigation: t!("view.mi_resources.investigate"),
        verify: t!("view.mi_resources.verify"),
    }
}
fn interval(
    row: &MssqlManagedInstanceResourceSample,
    layout: &MssqlManagedInstanceResourceLayout,
) -> ManagedInstanceResourceInterval {
    let fields = vec![
        field(
            "avg_cpu_percent",
            t!("view.mi_resources.field.cpu"),
            layout.cpu,
            &row.avg_cpu_percent,
            DisplayKind::Number,
        ),
        field(
            "virtual_core_count",
            t!("view.mi_resources.field.cores"),
            layout.virtual_core_count,
            &row.virtual_core_count,
            DisplayKind::Number,
        ),
        field(
            "reserved_storage_mb",
            t!("view.mi_resources.field.reserved"),
            layout.reserved_storage_mb,
            &row.reserved_storage_mb,
            DisplayKind::Number,
        ),
        field(
            "storage_space_used_mb",
            t!("view.mi_resources.field.used"),
            layout.storage_space_used_mb,
            &row.storage_space_used_mb,
            DisplayKind::Number,
        ),
        field(
            "io_request",
            t!("view.mi_resources.field.requests"),
            layout.io_request,
            &row.io_request,
            DisplayKind::Number,
        ),
        field(
            "io_bytes_read",
            t!("view.mi_resources.field.read"),
            layout.io_bytes_read,
            &row.io_bytes_read,
            DisplayKind::Number,
        ),
        field(
            "io_bytes_written",
            t!("view.mi_resources.field.written"),
            layout.io_bytes_written,
            &row.io_bytes_written,
            DisplayKind::Number,
        ),
        field(
            "resource_type",
            t!("view.mi_resources.field.resource_type"),
            layout.resource_type,
            &row.resource_type,
            DisplayKind::Text,
        ),
        field(
            "resource_name",
            t!("view.mi_resources.field.resource_name"),
            layout.resource_name,
            &row.resource_name,
            DisplayKind::Text,
        ),
        field("sku", t!("view.mi_resources.field.tier"), layout.sku, &row.sku, DisplayKind::Text),
        field(
            "hardware_generation",
            t!("view.mi_resources.field.hardware"),
            layout.hardware_generation,
            &row.hardware_generation,
            DisplayKind::Text,
        ),
    ];
    ManagedInstanceResourceInterval { started_at: row.started_at.clone(), ended_at: row.ended_at.clone(), fields }
}
#[derive(Clone, Copy)]
enum DisplayKind {
    Number,
    Text,
}
fn field(
    key: &str,
    label: String,
    availability: MssqlCatalogAvailability,
    native: &Option<String>,
    kind: DisplayKind,
) -> ManagedInstanceResourceField {
    ManagedInstanceResourceField {
        key: key.into(),
        label,
        availability,
        native_value: native.clone(),
        missing_reason: native.is_none().then(|| match availability {
            MssqlCatalogAvailability::Available => t!("ui.memory_failures.not_reported"),
            MssqlCatalogAvailability::Unsupported => t!("view.mi_resources.field_unsupported"),
            MssqlCatalogAvailability::PermissionDenied => t!("view.mi_resources.field_permission"),
            MssqlCatalogAvailability::Unknown => t!("view.mi_resources.field_unknown"),
        }),
        value: native.as_ref().map(|value| {
            if matches!(kind, DisplayKind::Number) { format::digits(value.clone()) } else { value.clone() }
        }),
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
