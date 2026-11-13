//! Native PostgreSQL lineage and the observed server endpoint are distinct from the database used to reach them.
//! Physical replicas share a system identifier; loopback or overlapping networks can make endpoint tuples ambiguous.
//! This is not a proven physical-host UUID. Unix sockets or denied control access produce an explicit error,
//! never a connection-name, role, PID, timeline or start-time fallback.
use super::*;
use culprit_core::{Engine, telemetry::*};

pub(crate) struct Instance {
    pub identity: SharedResourceIdentity,
    pub observed_at: String,
}

const MIN_SUPPORTED_VERSION: u32 = 100_000;

pub(crate) async fn read<R: QueryRead>(reader: &mut R, version: u32) -> Result<Instance> {
    if version < MIN_SUPPORTED_VERSION {
        return Err(culprit_core::Error::new(
            culprit_core::ErrorCode::Unsupported,
            culprit_core::t!("collection.unsupported"),
        ));
    }
    let row = one(
        reader,
        &format!(
            "SELECT pg_catalog.host(pg_catalog.inet_server_addr()) AS address,pg_catalog.inet_server_port()::text AS port,\
         system_identifier::text AS system_identifier,{} AS observed_at FROM pg_catalog.pg_control_system()",
            iso("pg_catalog.clock_timestamp()")
        ),
    )
    .await?;
    let address = text(&row, "address")?;
    let address = address.parse::<std::net::IpAddr>().map_err(|_| invalid())?.to_string();
    let port = integer(&row, "port")?;
    if port == 0 || port > u64::from(u16::MAX) {
        return Err(invalid());
    }
    // pg_control_system exposes the native bit pattern as signed bigint. It is an identifier, not a counter.
    let system = text(&row, "system_identifier")?.parse::<i64>().map_err(|_| invalid())?;
    if system == 0 {
        return Err(invalid());
    }
    let identity = SharedResourceIdentity {
        engine: Engine::Postgres,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new(system.to_string()).qualified("kind", "postgres-system-identifier"),
        native: NativeIdentity::new(address).qualified("port", port.to_string()),
    };
    let observed_at = canonical_time(&text(&row, "observed_at")?)?;
    if !identity.is_valid() {
        return Err(invalid());
    }
    Ok(Instance { identity, observed_at })
}

#[cfg(test)]
mod tests;
