//! One informational interpretation of the original six-field pool Settings source.
use culprit_core::{PoolSettingField, PoolSettingsReport, Result, SourceState, format, t, telemetry::*};

pub struct PoolSettingsEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a SettingObservation],
    pub read_complete: bool,
}

pub fn explain(evidence: PoolSettingsEvidence<'_>) -> Result<PoolSettingsReport> {
    let membership = validate_mssql_pool_settings_read(evidence.source, evidence.observations, evidence.read_complete)?;
    let native = evidence.source.observation.snapshot().ok_or_else(invalid)?;

    let source = native.sources.first().ok_or_else(invalid)?;
    let mut report = unavailable(&culprit_core::message!("view.pool_settings.unavailable"));
    report.target = Some(evidence.source.target.clone());
    report.elastic_pool = membership.elastic_pool.clone();
    report.elastic_pool_error = membership.elastic_pool_error;
    report.member_database = Some(membership.database.database.clone());
    report.logical_server = Some(membership.member.id.clone());
    report.context =
        Some(ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(evidence.source)?) });
    report.native_source = Some(source.clone());
    report.source_state =
        if !evidence.read_complete && source.records > 0 { SourceState::Partial } else { source.state };
    report.native_rows = source.records;
    report.observed_rows = evidence.observations.len() as u32;
    report.read_complete = evidence.read_complete;
    report.observed_at = membership.point.as_ref().map(|point| point.observed_at.clone());
    report.limitations = limitations();
    if membership.point.is_none() {
        return Ok(report);
    }
    report.summary = if evidence.read_complete {
        if membership.elastic_pool_error.is_some() {
            t!("view.pool_settings.summary.membership_unavailable")
        } else if membership.elastic_pool.is_none() {
            t!("view.pool_settings.summary.unpooled")
        } else {
            t!("view.pool_settings.summary")
        }
    } else {
        t!(
            "view.pool_settings.summary.partial",
            observed = format::digits(report.observed_rows.to_string()),
            native = format::digits(report.native_rows.to_string())
        )
    };
    for name in MSSQL_POOL_SETTING_NAMES {
        if let Some(row) = evidence.observations.iter().find(|row| row.name == name) {
            report.fields.push(field(row)?);
        }
    }
    Ok(report)
}

pub fn explain_captured(read: &CapturedPoolSettingsRead) -> Result<PoolSettingsReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(PoolSettingsEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}

pub fn unavailable(reason: &culprit_core::i18n::Message) -> PoolSettingsReport {
    PoolSettingsReport {
        target: None,
        elastic_pool: None,
        elastic_pool_error: None,
        member_database: None,
        logical_server: None,
        context: None,
        native_source: None,
        source_state: SourceState::Unavailable,
        native_rows: 0,
        observed_rows: 0,
        read_complete: false,
        observed_at: None,
        title: t!("view.pool_settings.title"),
        summary: reason.render(),
        fields: vec![],
        limitations: limitations(),
        investigation: t!("view.pool_settings.investigate"),
        verify: t!("view.pool_settings.verify"),
    }
}
fn limitations() -> Vec<String> {
    vec![
        t!("view.pool_settings.limit.scope"),
        t!("view.pool_settings.limit.meaning"),
        t!("view.pool_settings.limit.membership"),
        t!("view.pool_settings.limit.clock"),
    ]
}
fn field(row: &SettingObservation) -> Result<PoolSettingField> {
    let (label, explanation) = match row.name.as_str() {
        "min_cpu" => (t!("view.pool_settings.field.min_cpu"), t!("view.pool_settings.meaning.min_cpu")),
        "max_cpu" => (t!("view.pool_settings.field.max_cpu"), t!("view.pool_settings.meaning.max_cpu")),
        "cap_cpu" => (t!("view.pool_settings.field.cap_cpu"), t!("view.pool_settings.meaning.cap_cpu")),
        "min_memory" => (t!("view.pool_settings.field.min_memory"), t!("view.pool_settings.meaning.min_memory")),
        "max_memory" => (t!("view.pool_settings.field.max_memory"), t!("view.pool_settings.meaning.max_memory")),
        "pool_max_io" => (t!("view.pool_settings.field.pool_max_io"), t!("view.pool_settings.meaning.pool_max_io")),
        _ => return Err(invalid()),
    };
    let value = row
        .value
        .as_deref()
        .map(|value| {
            if row.name == "pool_max_io" {
                Ok(t!("view.pool_settings.iops", value = format::digits(value.to_owned())))
            } else {
                value.parse::<u32>().map(|value| format::percent_value(f64::from(value))).map_err(|_| invalid())
            }
        })
        .transpose()?;
    Ok(PoolSettingField {
        key: row.name.clone(),
        label,
        native_value: row.value.clone(),
        unit: row.unit.clone().ok_or_else(invalid)?,
        value,
        explanation,
    })
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
