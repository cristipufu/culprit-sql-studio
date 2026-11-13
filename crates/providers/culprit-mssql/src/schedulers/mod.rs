//! Shared SQLOS scheduler evidence, not a count of this database's requests or proven CPU/THREADPOOL causes.
//! Native field availability selects the projection; a failed query never triggers a broader fallback.
//! https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-schedulers-transact-sql
mod fields;

use crate::read::{NativeRead, Parameter, ReadError, invalid_data};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeSet;

pub(crate) const NATIVE_OBJECT: &str = "sys.dm_os_schedulers";
pub(crate) const MAX_SCHEDULERS: usize = 4096;
const IDENTITY_COLUMN: &str = "scheduler_id";

/// Both interactive and collector owners pass their bounded reader and retain responsibility for close/finish.
/// SQL Server 2022+ needs VIEW SERVER PERFORMANCE STATE; older SQL Server/MI needs VIEW SERVER STATE.
/// Azure SQL permissions vary by service objective; pooled databases require a server-level state reader/admin.
pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<Vec<NativeCounterSet>> {
    let limit = limit.min(MAX_SCHEDULERS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let columns = fields::Columns::read(reader).await?;
    let query = format!(
        "SELECT /* culprit */ TOP (@P1) {} FROM {NATIVE_OBJECT} ORDER BY [{IDENTITY_COLUMN}] OPTION(MAXDOP 1)",
        columns.projection()
    );
    let rows = reader
        .query(&query, vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let samples = rows.iter().map(|row| columns.decode(row)).collect::<Result<Vec<_>>>()?;
    let mut identities = BTreeSet::new();
    if samples.iter().any(|sample| !identities.insert(sample.identity.clone())) {
        return Err(invalid_data());
    }
    Ok(samples)
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
pub(crate) use fields::all_columns as fixture_columns;
