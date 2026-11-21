//! One informational interpretation of recorded selected-database properties; no configuration advice is inferred.
use culprit_core::{
    DatabasePropertiesReport, DatabasePropertyField, Result, Snapshot, SourceState, format, t, telemetry::*,
};

mod origin;

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<DatabasePropertiesReport>> {
    if let Some(read) = snapshot.operations.database_properties.as_deref() {
        return explain(read).map(Some);
    }
    if let Some(source) = snapshot.sources.iter().find(|source| source.source == DATABASE_PROPERTIES_SOURCE) {
        let mut report = unavailable();
        report.source_state = Some(match source.state {
            SourceState::Collected | SourceState::Empty | SourceState::Partial => SourceState::Partial,
            state => state,
        });
        return Ok(Some(report));
    }
    Ok(None)
}

pub fn explain(read: &DatabasePropertiesRead) -> Result<DatabasePropertiesReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    let mut report = unavailable();
    report.target = Some(read.target.clone());
    report.context = Some(read.context.clone());
    report.native_source = Some(read.source.clone());
    report.read_complete = Some(read.read_complete);
    report.source_state =
        Some(if !read.read_complete && matches!(read.source.state, SourceState::Collected | SourceState::Partial) {
            SourceState::Partial
        } else {
            read.source.state
        });
    if !read.read_complete {
        report.limitations.push(t!("view.database_properties.limit.read"));
    }
    let Some(point) = read.observation.as_deref() else { return Ok(report) };
    report.observation_key = Some(point.key()?);
    report.observed_at = Some(point.observed_at.clone());
    report.native_database_id = Some(point.native_database_id.clone());
    report.summary = t!("view.database_properties.summary.recorded");
    report.fields = fields(point);
    report.limitations.extend(origin::limitations(point));
    if let Some(note) = read.target.engine().spec().integrity_history_note() {
        report.limitations.push(culprit_core::i18n::t(note, &[]));
    }
    Ok(report)
}

fn unavailable() -> DatabasePropertiesReport {
    DatabasePropertiesReport {
        target: None,
        context: None,
        native_source: None,
        source_state: Some(SourceState::Unavailable),
        read_complete: None,
        observation_key: None,
        observed_at: None,
        native_database_id: None,
        title: t!("view.database_properties.title"),
        summary: t!("view.database_properties.unavailable"),
        limitations: vec![
            t!("view.database_properties.limit.scope"),
            t!("view.database_properties.limit.state"),
            t!("view.database_properties.limit.query_store"),
            t!("view.database_properties.limit.advice"),
            t!("view.database_properties.limit.features"),
        ],
        fields: vec![],
    }
}

fn fields(point: &DatabasePropertiesObservation) -> Vec<DatabasePropertyField> {
    use DatabasePropertyCodeKind as C;
    let mut fields = vec![
        coded(
            "catalog_state",
            t!("view.database_properties.field.catalog_state"),
            &point.catalog_state,
            point.state(C::CatalogState),
        ),
        coded(
            "user_access",
            t!("view.database_properties.field.user_access"),
            &point.user_access,
            point.state(C::UserAccess),
        ),
        field(
            "updateability",
            t!("view.database_properties.field.updateability"),
            point.writable().map(|value| {
                if value { t!("view.database_properties.read_write") } else { t!("view.database_properties.read_only") }
            }),
            None,
            point.updateability.clone(),
        ),
        field(
            "catalog_read_only",
            t!("view.database_properties.field.catalog_read_only"),
            yes_no(point.catalog_read_only),
            None,
            None,
        ),
        field(
            "last_good_checkdb_without_offset",
            t!("view.database_properties.field.last_good_checkdb"),
            match point.has_recorded_checkdb() {
                Some(true) => point.last_good_checkdb_without_offset.clone(),
                Some(false) => Some(t!("view.database_properties.checkdb_not_recorded")),
                None => None,
            },
            None,
            None,
        ),
        field("collation", t!("view.database_properties.field.collation"), point.collation.clone(), None, None),
        field(
            "compatibility_level",
            t!("view.database_properties.field.compatibility_level"),
            point.compatibility_level.map(|value| format::digits(value.to_string())),
            None,
            None,
        ),
        coded(
            "snapshot_isolation",
            t!("view.database_properties.field.snapshot_isolation"),
            &point.snapshot_isolation,
            point.state(C::SnapshotIsolation),
        ),
        field(
            "read_committed_snapshot",
            t!("view.database_properties.field.read_committed_snapshot"),
            enabled(point.read_committed_snapshot),
            None,
            None,
        ),
        field(
            "auto_create_statistics",
            t!("view.database_properties.field.auto_create_statistics"),
            enabled(point.auto_create_statistics),
            None,
            None,
        ),
        field(
            "auto_update_statistics",
            t!("view.database_properties.field.auto_update_statistics"),
            enabled(point.auto_update_statistics),
            None,
            None,
        ),
        field(
            "auto_update_statistics_async",
            t!("view.database_properties.field.auto_update_statistics_async"),
            enabled(point.auto_update_statistics_async),
            None,
            None,
        ),
        field(
            "forced_parameterization",
            t!("view.database_properties.field.forced_parameterization"),
            enabled(point.forced_parameterization),
            None,
            None,
        ),
        field(
            "query_store_enabled",
            t!("view.database_properties.field.query_store_enabled"),
            enabled(point.query_store_enabled),
            None,
            None,
        ),
        coded(
            "page_verify",
            t!("view.database_properties.field.page_verify"),
            &point.page_verify,
            point.state(C::PageVerify),
        ),
        coded(
            "recovery_model",
            t!("view.database_properties.field.recovery_model"),
            &point.recovery_model,
            point.state(C::RecoveryModel),
        ),
        field(
            "target_recovery_time_seconds",
            t!("view.database_properties.field.target_recovery_time_seconds"),
            point.target_recovery_time_seconds.map(|value| format::digits(value.to_string())),
            None,
            None,
        ),
        field("auto_shrink", t!("view.database_properties.field.auto_shrink"), enabled(point.auto_shrink), None, None),
        field("in_standby", t!("view.database_properties.field.in_standby"), yes_no(point.in_standby), None, None),
        coded(
            "delayed_durability",
            t!("view.database_properties.field.delayed_durability"),
            &point.delayed_durability,
            point.state(C::DelayedDurability),
        ),
        field(
            "broker_enabled",
            t!("view.database_properties.field.broker_enabled"),
            enabled(point.broker_enabled),
            None,
            None,
        ),
        coded(
            "containment",
            t!("view.database_properties.field.containment"),
            &point.containment,
            point.state(C::Containment),
        ),
        field("encrypted", t!("view.database_properties.field.encrypted"), enabled(point.encrypted), None, None),
        field(
            "ledger_enabled",
            t!("view.database_properties.field.ledger_enabled"),
            yes_no(point.ledger_enabled),
            None,
            None,
        ),
        field(
            "change_feed_enabled",
            t!("view.database_properties.field.change_feed_enabled"),
            enabled(point.change_feed_enabled),
            None,
            None,
        ),
        field("published", t!("view.database_properties.field.published"), yes_no(point.published), None, None),
        field("subscribed", t!("view.database_properties.field.subscribed"), yes_no(point.subscribed), None, None),
        field(
            "merge_published",
            t!("view.database_properties.field.merge_published"),
            yes_no(point.merge_published),
            None,
            None,
        ),
        field("distributor", t!("view.database_properties.field.distributor"), yes_no(point.distributor), None, None),
        field(
            "accelerated_recovery_enabled",
            t!("view.database_properties.field.accelerated_recovery_enabled"),
            enabled(point.accelerated_recovery_enabled),
            None,
            None,
        ),
    ];
    fields.extend(origin::fields(point));
    fields
}
fn field(
    key: &str,
    label: String,
    value: Option<String>,
    native_code: Option<String>,
    native_description: Option<String>,
) -> DatabasePropertyField {
    DatabasePropertyField { key: key.into(), label, value, native_code, native_description }
}
fn coded(
    key: &str,
    label: String,
    native: &DatabasePropertyCode,
    meaning: Option<DatabasePropertyState>,
) -> DatabasePropertyField {
    field(
        key,
        label,
        meaning.map(state),
        native.code.map(|value| format::digits(value.to_string())),
        native.description.clone(),
    )
}
fn enabled(value: Option<bool>) -> Option<String> {
    value.map(
        |value| if value { t!("view.database_properties.enabled") } else { t!("view.database_properties.disabled") },
    )
}
fn yes_no(value: Option<bool>) -> Option<String> {
    value.map(|value| if value { t!("view.database_properties.yes") } else { t!("view.database_properties.no") })
}
fn state(state: DatabasePropertyState) -> String {
    use DatabasePropertyState::*;
    match state {
        Online => t!("view.database_properties.state.online"),
        Restoring => t!("view.database_properties.state.restoring"),
        Recovering => t!("view.database_properties.state.recovering"),
        RecoveryPending => t!("view.database_properties.state.recovery_pending"),
        Suspect => t!("view.database_properties.state.suspect"),
        Emergency => t!("view.database_properties.state.emergency"),
        Offline => t!("view.database_properties.state.offline"),
        Copying => t!("view.database_properties.state.copying"),
        OfflineSecondary => t!("view.database_properties.state.offline_secondary"),
        MultiUser => t!("view.database_properties.state.multi_user"),
        SingleUser => t!("view.database_properties.state.single_user"),
        RestrictedUser => t!("view.database_properties.state.restricted_user"),
        Off => t!("view.database_properties.disabled"),
        On => t!("view.database_properties.enabled"),
        TransitionToOff => t!("view.database_properties.state.transition_off"),
        TransitionToOn => t!("view.database_properties.state.transition_on"),
        NoVerification => t!("view.database_properties.state.no_verification"),
        TornPageDetection => t!("view.database_properties.state.torn_page"),
        Checksum => t!("view.database_properties.state.checksum"),
        FullRecovery => t!("view.database_properties.state.full_recovery"),
        BulkLoggedRecovery => t!("view.database_properties.state.bulk_logged_recovery"),
        SimpleRecovery => t!("view.database_properties.state.simple_recovery"),
        Allowed => t!("view.database_properties.state.allowed"),
        Forced => t!("view.database_properties.state.forced"),
        NoContainment => t!("view.database_properties.state.not_contained"),
        PartialContainment => t!("view.database_properties.state.partially_contained"),
    }
}

#[cfg(test)]
mod tests;
