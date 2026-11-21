//! Native elastic-pool identity and retained history. The member database is a route and a consistency anchor;
//! it is never substituted for the pool. No undocumented incarnation or sampling period is invented.
pub(super) mod sample;

use super::*;
use crate::read::one;
use culprit_core::{Engine, source_id};

mod current;
pub(crate) use current::CurrentContext;
mod settings;
pub(crate) use settings::{Context as SettingsContext, read as read_settings};

const MEMBERSHIP_FROM: &str =
    "FROM sys.database_service_objectives s JOIN sys.databases d ON s.database_id=d.database_id WHERE d.name=DB_NAME()";
fn identity_query(extra_columns: &str) -> String {
    identity_projection(
        extra_columns,
        "membership.elastic_pool_name",
        &format!(" CROSS JOIN (SELECT s.elastic_pool_name {MEMBERSHIP_FROM}) membership"),
    )
}
fn identity_projection(extra_columns: &str, pool_name: &str, membership_relation: &str) -> String {
    format!(
        r#"SELECT /* culprit */ g.server_name,
 CONVERT(varchar(36),g.logical_database_guid) AS incarnation,
 CONVERT(varchar(36),g.physical_database_guid) AS physical_incarnation,
 CAST(DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS nvarchar(32)) AS updateability,
 {pool_name} AS pool_name,
 g.cpu_limit,g.dtu_limit,CONVERT(varchar(40),SYSUTCDATETIME(),126)+'Z' AS observed_at{extra_columns}
FROM sys.dm_user_db_resource_governance g{membership_relation}
WHERE g.database_id=DB_ID() AND g.database_name=DB_NAME()
OPTION(MAXDOP 1)"#
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pricing {
    Vcore,
    Dtu,
    Unknown,
}

#[derive(Clone)]
pub(crate) struct Context {
    pub identity: SharedResourceIdentity,
    member: NativeIdentity,
    pricing: Pricing,
}
// Membership equality excludes the separately checked observation clock.
impl PartialEq for Context {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.member == other.member && self.pricing == other.pricing
    }
}
impl Eq for Context {}

/// Even an unpooled member needs an exact physical route anchor. It is not substituted for pool identity.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Membership {
    pub pool: Option<Context>,
    pub route: NativeIdentity,
}

pub(crate) async fn identity<R: NativeRead>(io: &mut R) -> Result<(Membership, String)> {
    decode_membership(&one(io, &identity_query("")).await?)
}

fn decode_membership(row: &Rec) -> Result<(Membership, String)> {
    crate::read::require_primary(crate::read::database_writable(row)?)?;
    let observed_at = canonical_time(&text(row, "observed_at")?)?;
    let route = NativeIdentity::new(text(row, "server_name")?)
        .qualified("logical_database_guid", text(row, "incarnation")?)
        .qualified("physical_database_guid", text(row, "physical_incarnation")?);
    if !route.is_valid() {
        return Err(invalid_data());
    }
    let pool_name = match row.columns().find(|(key, _)| *key == "pool_name").map(|(_, value)| value) {
        Some(crate::value::Val::Null) => return Ok((Membership { pool: None, route }, observed_at)),
        Some(crate::value::Val::Text(name)) if !name.trim().is_empty() => name,
        _ => return Err(invalid_data()),
    };
    let context = Context {
        identity: SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new(text(row, "server_name")?).qualified("kind", "azure-sql-logical-server"),
            native: NativeIdentity::new(pool_name),
        },
        member: route.clone(),
        pricing: match (super::sample::number(row, "cpu_limit")?, super::sample::number(row, "dtu_limit")?) {
            (Some(_), None) => Pricing::Vcore,
            (None, Some(_)) => Pricing::Dtu,
            _ => Pricing::Unknown,
        },
    };
    if !context.identity.is_valid() {
        return Err(invalid_data());
    }
    Ok((Membership { pool: Some(context), route }, observed_at))
}

pub(crate) async fn inventory<R: NativeRead>(io: &mut R, window: &Window) -> Result<Inventory> {
    Inventory::shared(&super::inventory_row(io, window, &super::sql::POOL.inventory()).await?, window)
}

pub(crate) async fn windowed<R: NativeRead>(
    io: &mut R,
    context: &Context,
    window: &Window,
    after: Option<&str>,
    limit: u32,
) -> ReadResult<Vec<ResourceMeasurement>> {
    super::windowed_rows(io, window, after, limit, &super::sql::POOL.windowed())
        .await?
        .iter()
        .map(|row| sample::decode(row, context.pricing))
        .collect::<Result<Vec<_>>>()
        .map_err(ReadError::from)
}

pub(crate) const SOURCE: &str = source_id::POOL_RESOURCES;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
