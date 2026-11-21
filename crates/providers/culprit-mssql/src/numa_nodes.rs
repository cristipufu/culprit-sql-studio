//! Read-only fixed native node populations. No physical-memory, application-data or instrumentation access.
//! https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-nodes-transact-sql
//! https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-memory-nodes-transact-sql
use crate::read::{NativeRead, Parameter, ReadError, invalid_data};
use culprit_core::{Result, telemetry::*};
use std::collections::BTreeSet;
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    kind: MssqlNumaNodeKind,
    limit: usize,
) -> Result<Vec<NativeCounterSet>> {
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let fields = crate::native_counter_fields::Columns::read(reader, kind.object(), kind.fields()).await?;
    let query = format!(
        "SELECT /* culprit */ TOP (@P1) {} FROM {} ORDER BY [{}] OPTION(MAXDOP 1)",
        fields.projection(),
        kind.object(),
        kind.identity_column()
    );
    let rows = reader
        .query(&query, vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut identities = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let sample = fields.decode(row)?;
            let point = MssqlNumaNodePoint::from_sample(&sample)?;
            if point.kind() != kind || !identities.insert(sample.identity.clone()) {
                return Err(invalid_data());
            }
            Ok(sample)
        })
        .collect()
}
#[cfg(test)]
pub(crate) mod tests;
