//! One bounded SHOW read reuses the retained operational decoder; unavailable variables are never invented.
use super::{Kind, STATUS};
use crate::{
    native::invalid,
    read::QueryRead,
    vars::{self, VariableSet},
};
use culprit_core::{
    Result,
    telemetry::{OperationalObservation, TelemetryRecord},
};
use std::collections::BTreeSet;

/// Global instance counters, independent of Performance Schema consumers. FLUSH STATUS can reset these
/// independently of process uptime; no startup age or selected database owns their values.
/// https://dev.mysql.com/doc/refman/8.4/en/server-status-variables.html
/// https://mariadb.com/docs/server/server-management/variables-and-modes/server-status-variables
pub(crate) async fn read(reader: &mut impl QueryRead) -> Result<Vec<OperationalObservation>> {
    let names = STATUS.iter().filter(|spec| spec.connection_outcome).map(|spec| spec.name).collect::<Vec<_>>();
    let rows = vars::records(reader, VariableSet::Status, &names).await?;
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let name = super::text(row, "variable_name")?;
            if !names.iter().any(|expected| expected.eq_ignore_ascii_case(&name))
                || !seen.insert(name.to_ascii_lowercase())
            {
                return Err(invalid());
            }
            let TelemetryRecord::Operational(observation) = Kind::Status.decode(row, "")? else {
                return Err(invalid());
            };
            Ok(observation)
        })
        .collect()
}

#[cfg(test)]
mod tests;
