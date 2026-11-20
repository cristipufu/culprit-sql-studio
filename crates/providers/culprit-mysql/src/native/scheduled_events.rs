//! One native catalog reader serves explicit scans and retained Jobs capture. It never runs an event.
use super::{instance, integer, invalid, metadata, one, text};
use crate::{
    connect::{Connector, Flavor},
    read::QueryRead,
    rows::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use mysql_async::Value;
use std::collections::{BTreeMap, BTreeSet};
mod sql;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Configuration {
    scheduler: Option<MysqlEventSchedulerSetting>,
    time_zone: Option<String>,
    event_visibility: bool,
}

pub(crate) struct Context {
    connection: metadata::ConnectionContext,
    identity: instance::Identity,
    configuration: Configuration,
    observed_at: String,
}
impl Context {
    pub(crate) async fn read<R: QueryRead>(reader: &mut R, connector: &Connector) -> Result<Self> {
        let connection = metadata::connection_context(reader, connector).await?;
        if connection.connection_id == 0 {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        if !supported(connection.flavor, &connection.version) {
            return Err(unsupported());
        }
        let grantee = metadata::grantee(&connection.login)?;
        let query = sql::configuration(connection.flavor);
        let row = one(
            reader,
            &query,
            vec![Value::from(grantee.clone()), Value::from(grantee), Value::from(connection.database_name.clone())],
        )
        .await?;
        let configuration = Configuration {
            scheduler: scheduler(&row, "event_scheduler")?,
            time_zone: row.native_text("session_time_zone")?,
            event_visibility: super::flag(&row, "event_visibility")?,
        };
        let identity = instance::Platform::from_connection(&connection).identify(reader, &row).await?;
        let observed_at = canonical_time(&text(&row, "observed_at")?)?;
        if ObservationBounds::new(&connection.at, &observed_at).is_none() {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(Self { connection, identity, configuration, observed_at })
    }
    fn same_source(&self, after: &Self) -> bool {
        self.connection.same_source(&after.connection)
            && self.identity == after.identity
            && self.configuration == after.configuration
            && ObservationBounds::new(&self.observed_at, &after.connection.at).is_some()
    }
    fn completed(&self, after: &Self) -> Result<NativeReadContext> {
        if !self.same_source(after) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let owner = NativeReadContext {
            target: self.connection.database.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(self.identity.resource.clone()),
                epoch: Some(super::local_context::process_epoch(self.identity.started_second)),
            },
            observed_from: canonical_time(&self.connection.at)?,
            observed_until: after.observed_at.clone(),
            database_writable: Some(!self.connection.read_only),
        };
        owner.has_qualified_local_owner().then_some(owner).ok_or_else(invalid)
    }
    pub(crate) fn verifies(&self, after: &Self, source: &CollectionDescription) -> bool {
        self.same_source(after)
            && source.observation.snapshot().is_some_and(|snapshot| {
                ObservationBounds::new(&snapshot.observed_until, &after.connection.at).is_some()
            })
    }
}
fn supported(flavor: Flavor, version: &str) -> bool {
    match flavor {
        Flavor::Mysql => culprit_core::version_at_least(version, [5, 7, 0]),
        Flavor::Mariadb => super::local_context::has_mariadb_uid(version),
    }
}
fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported"))
}

pub(crate) struct Capture {
    pub snapshot: MysqlEventsSnapshot,
    pub context: Context,
}
pub(crate) async fn capture<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
    budget: &CollectionBudget,
) -> Result<Capture> {
    let limit = (budget.max_snapshot_rows as usize).min(MAX_MYSQL_SCHEDULED_EVENTS);
    if limit == 0 || limit > MAX_MYSQL_SCHEDULED_EVENTS {
        return Err(CollectionStop::RowLimit.error());
    }
    let before = Context::read(reader, connector).await?;
    let mut metadata = MysqlEventsMetadata {
        scheduler_setting: before.configuration.scheduler,
        scheduler_observed_at: before.observed_at.clone(),
        database_event_visibility: before.configuration.event_visibility,
        session_time_zone: before.configuration.time_zone.clone(),
        database_writable: !before.connection.read_only,
        layout: MysqlEventCatalogLayout::Available,
    };
    let mut events = Vec::new();
    let mut data_clocks = Vec::new();
    let coverage = match layout(reader).await {
        Ok(true) => match reader
            .read(
                &sql::events(before.connection.flavor),
                vec![Value::from(before.connection.database_name.clone()), Value::from((limit + 1) as u64)],
                limit + 1,
            )
            .await
        {
            Ok(rows) => {
                if rows.is_empty() {
                    return Err(invalid());
                }
                let mut seen = BTreeSet::new();
                let row_count = rows.len();
                for row in rows {
                    data_clocks.push(validate_data_owner(&row, &before)?);
                    if row.native_text("event_schema")?.is_none() {
                        if row_count != 1 {
                            return Err(invalid());
                        }
                        for field in [
                            "event_name",
                            "status",
                            "event_type",
                            "time_zone",
                            "interval_value",
                            "interval_field",
                            "execute_at",
                            "starts",
                            "ends",
                            "last_started",
                            "created",
                            "last_altered",
                        ] {
                            if row.native_text(field)?.is_some() {
                                return Err(invalid());
                            }
                        }
                        continue;
                    }
                    let event = decode(&row, &before.connection.database_name)?;
                    if !seen.insert(event.key()?) {
                        return Err(invalid());
                    }
                    events.push(event);
                }
                let truncated = events.len() > limit;
                let mut coverage = StateSourceCoverage::observed(
                    MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
                    events.len().min(limit) as u32,
                    !truncated
                        && metadata.database_event_visibility
                        && events.iter().all(MysqlScheduledEvent::understood),
                );
                if truncated {
                    coverage.stopped = Some(CollectionStop::RowLimit)
                }
                coverage
            }
            Err(error) => unavailable(error)?,
        },
        Ok(false) => {
            metadata.layout = MysqlEventCatalogLayout::Unsupported;
            unavailable(unsupported())?
        }
        Err(error) => {
            metadata.layout = if error.code == ErrorCode::PermissionDenied {
                MysqlEventCatalogLayout::PermissionDenied
            } else {
                MysqlEventCatalogLayout::ReadFailed
            };
            unavailable(error)?
        }
    };
    let after = Context::read(reader, connector).await?;
    let context = before.completed(&after)?;
    let bounds = ObservationBounds::new(&context.observed_from, &context.observed_until).ok_or_else(invalid)?;
    if data_clocks.iter().any(|at| !bounds.contains(at))
        || events.iter().any(|event| !bounds.contains(&event.observed_at))
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    events.truncate(limit);
    let observations = events
        .into_iter()
        .map(|event| {
            Ok(OperationalObservation {
                source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
                key: event.key()?,
                fact: OperationalFact::MysqlScheduledEvent(Box::new(event)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = MysqlEventsSnapshot { context, metadata, sources: vec![coverage], observations };
    if !snapshot.is_valid_read(true) {
        return Err(invalid());
    }
    if serde_json::to_vec(&snapshot).map_err(|_| invalid())?.len() > budget.total_bytes as usize {
        return Err(CollectionStop::ByteLimit.error());
    }
    Ok(Capture { snapshot, context: after })
}
fn unavailable(error: Error) -> Result<StateSourceCoverage> {
    if error.detail.is_some()
        || !matches!(
            error.code,
            ErrorCode::PermissionDenied | ErrorCode::Unsupported | ErrorCode::NotFound | ErrorCode::QueryFailed
        )
    {
        return Err(error);
    }
    Ok(StateSourceCoverage::unavailable(MYSQL_SCHEDULED_EVENTS_SOURCE.into(), error.code, None))
}
async fn layout<R: QueryRead>(reader: &mut R) -> Result<bool> {
    let rows = reader.read(&sql::layout(), vec![], sql::COLUMNS.len() + 1).await?;
    let mut actual = BTreeMap::new();
    for row in rows {
        let name = text(&row, "column_name")?.to_ascii_uppercase();
        let kind = text(&row, "data_type")?.to_ascii_lowercase();
        if actual.insert(name, kind).is_some() {
            return Err(invalid());
        }
    }
    Ok(actual.len() == sql::COLUMNS.len()
        && sql::COLUMNS.iter().all(|(name, date)| {
            actual.get(*name).is_some_and(|kind| {
                if *date {
                    matches!(kind.as_str(), "datetime" | "timestamp")
                } else {
                    matches!(kind.as_str(), "varchar" | "char" | "enum")
                }
            })
        }))
}
fn scheduler(row: &Rec, field: &str) -> Result<Option<MysqlEventSchedulerSetting>> {
    Ok(row.native_text(field)?.map(|value| match value.as_str() {
        "ON" => MysqlEventSchedulerSetting::On,
        "OFF" => MysqlEventSchedulerSetting::Off,
        "DISABLED" => MysqlEventSchedulerSetting::Disabled,
        _ => MysqlEventSchedulerSetting::Unknown,
    }))
}
fn validate_data_owner(row: &Rec, before: &Context) -> Result<String> {
    let resource = instance::resource(before.connection.flavor, &text(row, "source_member")?)?;
    if resource != before.identity.resource
        || integer(row, "source_connection_id")? != before.connection.connection_id
        || super::flag(row, "source_read_only")? != before.connection.read_only
        || row.native_text("source_time_zone")? != before.configuration.time_zone
        || scheduler(row, "source_scheduler")? != before.configuration.scheduler
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    canonical_time(&text(row, "observed_at")?)
}
fn decode(row: &Rec, database: &str) -> Result<MysqlScheduledEvent> {
    let schema = text(row, "event_schema")?;
    if schema != database {
        return Err(CollectionStop::SourceChanged.error());
    }
    let event = MysqlScheduledEvent {
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        schema,
        name: text(row, "event_name")?,
        status: row.native_text("status")?.map(|value| match value.as_str() {
            "ENABLED" => MysqlEventStatus::Enabled,
            "DISABLED" => MysqlEventStatus::Disabled,
            "REPLICA_SIDE_DISABLED" => MysqlEventStatus::ReplicaSideDisabled,
            "SLAVESIDE_DISABLED" => MysqlEventStatus::SlavesideDisabled,
            _ => MysqlEventStatus::Unknown,
        }),
        event_type: row.native_text("event_type")?.map(|value| match value.as_str() {
            "ONE TIME" => MysqlEventType::OneTime,
            "RECURRING" => MysqlEventType::Recurring,
            _ => MysqlEventType::Unknown,
        }),
        time_zone: row.native_text("time_zone")?,
        interval_value: row.native_text("interval_value")?,
        interval_field: row.native_text("interval_field")?,
        execute_at_without_offset: row.native_text("execute_at")?,
        starts_without_offset: row.native_text("starts")?,
        ends_without_offset: row.native_text("ends")?,
        last_started_without_offset: row.native_text("last_started")?,
        created_without_offset: row.native_text("created")?,
        altered_without_offset: row.native_text("last_altered")?,
    };
    event.is_valid().then_some(event).ok_or_else(invalid)
}
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
