//! A scan retains the same fully drained native acquisition used for its legacy replica projection.
use super::*;
use culprit_core::{Error, ErrorCode, Platform};
use std::time::SystemTime;

pub(crate) struct Acquired {
    pub replicas: Vec<Replica>,
    pub snapshot: ReplicationSnapshot,
    pub complete_visibility: bool,
    pub profile_unavailable: bool,
}

struct Context {
    version: u32,
    standby: bool,
    database_oid: String,
    full_visibility: bool,
}

pub(crate) async fn acquire<R: QueryRead>(reader: &mut R, platform: Option<Platform>) -> Result<Acquired> {
    let from = SystemTime::now();
    let before = context(reader).await?;
    let profile_unavailable = before.standby && platform == Some(Platform::AmazonAurora);
    let observations = if profile_unavailable {
        // Keep the existing Aurora standby policy; PostgreSQL recovery functions do not establish Aurora apply lag.
        Vec::new()
    } else {
        let rows = reader
            .query(
                &query(before.version, before.standby, &before.database_oid, MAX_REPLICATION_OBSERVATIONS + 1)?,
                MAX_REPLICATION_OBSERVATIONS + 1,
            )
            .await?;
        if rows.len() > MAX_REPLICATION_OBSERVATIONS {
            return Err(CollectionStop::RowLimit.error());
        }
        rows.iter().map(decode).collect::<Result<Vec<_>>>()?
    };
    for row in &observations {
        validate_context(row, &before)?;
    }
    let complete_visibility = before.full_visibility && observations.iter().all(|row| {
        matches!(&row.fact, ReplicationFact::Native(sample) if sample.measurements.properties.get("full_visibility").map(String::as_str) == Some("true"))
    });
    let sources = super::sources(before.standby)
        .into_iter()
        .map(|source| {
            if profile_unavailable {
                return StateSourceCoverage::not_applicable(source);
            }
            let records = observations.iter().filter(|row| row.source == source).count() as u32;
            StateSourceCoverage::observed(source, records, complete_visibility)
        })
        .collect();
    let snapshot = ReplicationSnapshot {
        context: ReplicationReadContext::collector(from, SystemTime::now())?,
        sources,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(invalid());
    }
    let replicas = snapshot
        .observations
        .iter()
        .filter(|row| belongs_to_legacy_projection(row, &before.database_oid))
        .filter_map(|row| project(&row.fact))
        .collect();
    Ok(Acquired { replicas, snapshot, complete_visibility, profile_unavailable })
}

fn validate_context(row: &ReplicationObservation, context: &Context) -> Result<()> {
    let ReplicationFact::Native(sample) = &row.fact else { return Err(invalid()) };
    let properties = &sample.measurements.properties;
    if properties.get("read_database_oid") != Some(&context.database_oid)
        || properties.get("server_version_num") != Some(&context.version.to_string())
        || properties.get("read_standby").map(String::as_str) != Some(if context.standby { "true" } else { "false" })
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}

fn belongs_to_legacy_projection(row: &ReplicationObservation, database_oid: &str) -> bool {
    let ReplicationFact::Native(sample) = &row.fact else { return false };
    let properties = &sample.measurements.properties;
    !(properties.get("kind").map(String::as_str) == Some("sender")
        && properties.get("logical").map(String::as_str) == Some("true")
        && properties.get("database_oid").is_some_and(|observed| observed != database_oid))
}

async fn context<R: QueryRead>(reader: &mut R) -> Result<Context> {
    let row = one(
        reader,
        "SELECT pg_catalog.pg_is_in_recovery() AS standby,\
         (SELECT oid::text FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()) AS database_oid,\
         pg_catalog.current_setting('server_version_num') AS version,\
         (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)\
             OR pg_catalog.pg_has_role(current_user,'pg_read_all_stats','USAGE') AS full_visibility",
    )
    .await?;
    let version = u32::try_from(integer(&row, "version")?).map_err(|_| invalid())?;
    if version < 100_000 {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let database_oid = integer(&row, "database_oid")?;
    let database_oid = u32::try_from(database_oid).ok().filter(|oid| *oid > 0).ok_or_else(invalid)?.to_string();
    Ok(Context {
        version,
        standby: flag(&row, "standby")?,
        database_oid,
        full_visibility: flag(&row, "full_visibility")?,
    })
}

#[cfg(test)]
mod tests;
