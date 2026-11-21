//! One native Group Replication layout/decoder for capture and explicit buffered consumers.
//! These unindexed Performance Schema tables describe the observer's group view, not an application database.
//! LIMIT bounds retained rows, not native enumeration work. No plugin/configuration mutation is issued.
//! https://dev.mysql.com/doc/refman/8.4/en/performance-schema-replication-group-members-table.html
//! https://dev.mysql.com/doc/refman/8.4/en/performance-schema-replication-group-member-stats-table.html
use super::{instance, invalid, one, text};
use crate::{connect::Flavor, read::QueryRead, rows::Rec};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use mysql_async::Value;
use std::collections::{BTreeMap, BTreeSet};

const MEMBERS: &str = "replication_group_members";
const STATISTICS: &str = "replication_group_member_stats";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    columns: BTreeMap<String, BTreeSet<String>>,
}
impl Layout {
    async fn read(reader: &mut impl QueryRead) -> Result<Self> {
        let rows = reader
            .read(
                "SELECT TABLE_NAME AS table_name, COLUMN_NAME AS column_name
            FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='performance_schema'
            AND TABLE_NAME IN ('replication_group_members','replication_group_member_stats') LIMIT 65",
                vec![],
                65,
            )
            .await?;
        if rows.len() > 64 {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut columns = BTreeMap::<String, BTreeSet<String>>::new();
        for row in rows {
            let table = text(&row, "table_name")?;
            if !matches!(table.as_str(), MEMBERS | STATISTICS)
                || !columns.entry(table).or_default().insert(text(&row, "column_name")?.to_ascii_uppercase())
            {
                return Err(invalid());
            }
        }
        Ok(Self { columns })
    }
    fn has(&self, table: &str, column: &str) -> bool {
        self.columns.get(table).is_some_and(|set| set.contains(column))
    }
    pub(crate) fn members(&self) -> bool {
        ["CHANNEL_NAME", "MEMBER_ID", "MEMBER_STATE"].into_iter().all(|field| self.has(MEMBERS, field))
    }
    pub(crate) fn statistics(&self) -> bool {
        [
            "CHANNEL_NAME",
            "MEMBER_ID",
            "VIEW_ID",
            "COUNT_TRANSACTIONS_IN_QUEUE",
            "COUNT_TRANSACTIONS_CHECKED",
            "COUNT_CONFLICTS_DETECTED",
            "COUNT_TRANSACTIONS_ROWS_VALIDATING",
        ]
        .into_iter()
        .all(|field| self.has(STATISTICS, field))
    }
    fn optional(&self, table: &str, column: &str, alias: &str) -> String {
        format!("{} AS {alias}", if self.has(table, column) { column } else { "NULL" })
    }
    pub(crate) fn query(&self, kind: Kind) -> String {
        let mut fields = vec![
            "CHANNEL_NAME AS channel".into(),
            "MEMBER_ID AS member_id".into(),
            "@@GLOBAL.server_uuid AS observer_id".into(),
            "@@GLOBAL.group_replication_group_name AS group_id".into(),
            "DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at".into(),
        ];
        let table = match kind {
            Kind::Members => {
                fields.push("MEMBER_STATE AS member_state".into());
                fields.push(self.optional(MEMBERS, "MEMBER_ROLE", "member_role"));
                fields.push(self.optional(MEMBERS, "MEMBER_VERSION", "member_version"));
                MEMBERS
            }
            Kind::Statistics => {
                fields.push("VIEW_ID AS view_id".into());
                fields.extend(Counter::ALL.map(|field| self.optional(STATISTICS, field.native(), field.alias())));
                STATISTICS
            }
        };
        format!("SELECT {} FROM performance_schema.{table} LIMIT ?", fields.join(", "))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub instance: instance::Identity,
    pub metadata: MysqlGroupReplicationMetadata,
    pub layout: Layout,
}
impl Context {
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.instance == other.instance
            && self.layout == other.layout
            && self.metadata.same_configuration(&other.metadata)
    }
    pub(crate) fn available(&self, kind: Kind) -> bool {
        self.metadata.available(kind.source())
    }
}
pub(crate) async fn context(reader: &mut impl QueryRead) -> Result<(Context, String)> {
    let platform = instance::platform(reader).await?;
    if !supported(platform.flavor, &platform.version, platform.performance_schema) {
        return Err(unsupported());
    }
    let configuration = one(reader, "SELECT @@GLOBAL.server_uuid AS server_uuid,
        (SELECT PLUGIN_STATUS FROM information_schema.PLUGINS WHERE PLUGIN_NAME='group_replication') AS plugin_status,
        (SELECT VARIABLE_VALUE FROM performance_schema.global_variables WHERE VARIABLE_NAME='group_replication_group_name') AS group_id,
        DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at", vec![]).await?;
    let group_id = configuration
        .native_text("group_id")?
        .filter(|value| !value.is_empty())
        .map(|value| uuid(&value))
        .transpose()?;
    let instance = platform.identify(reader, &configuration).await?;
    let layout = Layout::read(reader).await?;
    let observed_at = text(&configuration, "observed_at")?;
    let metadata = MysqlGroupReplicationMetadata {
        observed_at: observed_at.clone(),
        group_id,
        plugin_status: configuration.native_text("plugin_status")?,
        members_layout: layout.members(),
        statistics_layout: layout.statistics(),
    };
    if !metadata.is_valid() {
        return Err(invalid());
    }
    Ok((Context { instance, metadata, layout }, observed_at))
}
pub(crate) fn supported(flavor: Flavor, version: &str, performance_schema: bool) -> bool {
    flavor == Flavor::Mysql && culprit_core::version_at_least(version, [5, 7, 17]) && performance_schema
}
pub(crate) fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::message!("provider.mysql.group_replication.unsupported"))
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Members,
    Statistics,
}
impl Kind {
    pub(crate) const ALL: [Self; 2] = [Self::Members, Self::Statistics];
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Members => MYSQL_GROUP_MEMBERS_SOURCE,
            Self::Statistics => MYSQL_GROUP_STATISTICS_SOURCE,
        }
    }
    pub(crate) fn decode(self, row: &Rec, context: &Context) -> Result<MysqlGroupReplicationPoint> {
        let group_id = uuid(&text(row, "group_id")?)?;
        if Some(&group_id) != context.metadata.group_id.as_ref()
            || uuid(&text(row, "observer_id")?)? != context.instance.resource.native.id
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let member_id = uuid(&text(row, "member_id")?)?;
        let channel = text(row, "channel")?;
        let observed_at = text(row, "observed_at")?;
        let point = match self {
            Self::Members => MysqlGroupReplicationPoint::Member(Box::new(MysqlGroupMember {
                group_id,
                member_id,
                channel,
                observed_at,
                state: text(row, "member_state")?,
                role: row.native_text("member_role")?.filter(|value| !value.is_empty()),
                version: row.native_text("member_version")?.filter(|value| !value.is_empty()),
            })),
            Self::Statistics => MysqlGroupReplicationPoint::Statistics(Box::new(MysqlGroupStatistics {
                group_id,
                member_id,
                channel,
                observed_at,
                view_id: row.native_text("view_id")?.filter(|value| !value.is_empty()),
                certification_queue: count(row, Counter::CertificationQueue)?,
                checked: count(row, Counter::Checked)?,
                conflicts: count(row, Counter::Conflicts)?,
                rows_validating: count(row, Counter::RowsValidating)?,
                applier_queue: count(row, Counter::ApplierQueue)?,
                remote_applied: count(row, Counter::RemoteApplied)?,
                local_proposed: count(row, Counter::LocalProposed)?,
                local_rollback: count(row, Counter::LocalRollback)?,
            })),
        };
        point.is_valid().then_some(point).ok_or_else(invalid)
    }
}
#[derive(Clone, Copy)]
enum Counter {
    CertificationQueue,
    Checked,
    Conflicts,
    RowsValidating,
    ApplierQueue,
    RemoteApplied,
    LocalProposed,
    LocalRollback,
}
impl Counter {
    const ALL: [Self; 8] = [
        Self::CertificationQueue,
        Self::Checked,
        Self::Conflicts,
        Self::RowsValidating,
        Self::ApplierQueue,
        Self::RemoteApplied,
        Self::LocalProposed,
        Self::LocalRollback,
    ];
    fn native(self) -> &'static str {
        match self {
            Self::CertificationQueue => "COUNT_TRANSACTIONS_IN_QUEUE",
            Self::Checked => "COUNT_TRANSACTIONS_CHECKED",
            Self::Conflicts => "COUNT_CONFLICTS_DETECTED",
            Self::RowsValidating => "COUNT_TRANSACTIONS_ROWS_VALIDATING",
            Self::ApplierQueue => "COUNT_TRANSACTIONS_REMOTE_IN_APPLIER_QUEUE",
            Self::RemoteApplied => "COUNT_TRANSACTIONS_REMOTE_APPLIED",
            Self::LocalProposed => "COUNT_TRANSACTIONS_LOCAL_PROPOSED",
            Self::LocalRollback => "COUNT_TRANSACTIONS_LOCAL_ROLLBACK",
        }
    }
    fn alias(self) -> &'static str {
        match self {
            Self::CertificationQueue => "certification_queue",
            Self::Checked => "checked",
            Self::Conflicts => "conflicts",
            Self::RowsValidating => "rows_validating",
            Self::ApplierQueue => "applier_queue",
            Self::RemoteApplied => "remote_applied",
            Self::LocalProposed => "local_proposed",
            Self::LocalRollback => "local_rollback",
        }
    }
}
fn count(row: &Rec, counter: Counter) -> Result<Option<String>> {
    Ok(row.unsigned(counter.alias())?.map(|value| value.to_string()))
}
fn uuid(value: &str) -> Result<String> {
    Ok(instance::resource(Flavor::Mysql, value)?.native.id)
}

pub(crate) struct Observations {
    pub rows: Vec<OperationalObservation>,
    pub coverage: Vec<StateSourceCoverage>,
}
pub(crate) async fn read(reader: &mut impl QueryRead, context: &Context, limit: usize) -> Result<Observations> {
    if limit == 0 || limit > MYSQL_GROUP_MAX_MEMBERS {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut observations = Observations { rows: Vec::new(), coverage: Vec::new() };
    for kind in Kind::ALL {
        if !context.available(kind) {
            observations.coverage.push(unavailable(kind, ErrorCode::Unsupported));
            continue;
        }
        let result = reader.read(&context.layout.query(kind), vec![Value::UInt((limit + 1) as u64)], limit + 1).await;
        let mut rows = match result {
            Ok(rows) => rows,
            Err(error) if error.code == ErrorCode::PermissionDenied && error.detail.is_none() => {
                observations.coverage.push(unavailable(kind, error.code));
                continue;
            }
            Err(error) => return Err(error),
        };
        let complete = rows.len() <= limit;
        rows.truncate(limit);
        let mut keys = BTreeSet::new();
        let mut decoded = Vec::new();
        for row in rows {
            let point = kind.decode(&row, context)?;
            let key = point.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            decoded.push(OperationalObservation {
                source: kind.source().into(),
                key,
                fact: OperationalFact::MysqlGroupReplication(Box::new(point)),
            });
        }
        let mut coverage = StateSourceCoverage::observed(kind.source().into(), decoded.len() as u32, complete);
        if !complete {
            coverage.stopped = Some(CollectionStop::RowLimit);
        }
        observations.coverage.push(coverage);
        observations.rows.extend(decoded);
    }
    Ok(observations)
}
fn unavailable(kind: Kind, error: ErrorCode) -> StateSourceCoverage {
    StateSourceCoverage {
        source: kind.source().into(),
        state: culprit_core::SourceState::Unavailable,
        records: 0,
        complete_scope: false,
        error_code: Some(error),
        stopped: None,
    }
}
#[cfg(test)]
pub(crate) mod tests;
