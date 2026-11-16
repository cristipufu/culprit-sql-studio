//! One native channel read for the app and export; server-wide channels never become database-specific lag.
mod fields;
#[cfg(test)]
mod tests;
use super::*;
use crate::connect::Flavor;
use culprit_core::{MeasurementScope, telemetry::*};
use std::collections::BTreeMap;
pub(crate) const SOURCE: &str = "replication-channels";
pub(crate) fn query(flavor: Flavor, version: &str) -> &'static str {
    let version = if flavor == Flavor::Mariadb { version.strip_prefix("5.5.5-").unwrap_or(version) } else { version };
    match flavor {
        Flavor::Mysql if culprit_core::version_at_least(version, [8, 0, 22]) => "SHOW REPLICA STATUS",
        Flavor::Mysql => "SHOW SLAVE STATUS",
        Flavor::Mariadb if culprit_core::version_at_least(version, [10, 5, 1]) => "SHOW ALL REPLICAS STATUS",
        Flavor::Mariadb => "SHOW ALL SLAVES STATUS",
    }
}
pub(crate) async fn read(
    reader: &mut impl QueryRead,
    flavor: Flavor,
    version: &str,
) -> Result<Vec<ReplicationObservation>> {
    let rows = crate::rows::query(reader, query(flavor, version)).await?;
    let mut keys = std::collections::BTreeSet::new();
    rows.iter()
        .map(|row| {
            let point = observation(row)?;
            if !keys.insert(point.fact.key()) {
                return Err(invalid());
            }
            Ok(point)
        })
        .collect()
}

pub(crate) fn decode(row: &Rec) -> Result<TelemetryRecord> {
    observation(row).map(TelemetryRecord::Replication)
}

fn observation(row: &Rec) -> Result<ReplicationObservation> {
    let properties = fields::properties(row)?;
    let channel = properties.get("channel").cloned().ok_or_else(invalid)?;
    let set = NativeCounterSet {
        identity: NativeIdentity::new(serde_json::to_string(&channel).map_err(|_| invalid())?),
        scope: MeasurementScope::Server,
        properties,
        counters: fields::counters(row)?,
    };
    let fact = ReplicationFact::Native(Box::new(NativeReplicationSample::new(set)?));
    Ok(ReplicationObservation { source: SOURCE.into(), fact })
}
