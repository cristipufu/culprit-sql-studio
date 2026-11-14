//! One database-scoped native relation supplies scan projections and streamed index observations.
mod layout;
mod projection;
pub(crate) mod targeted;
use crate::{
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_integer, optional_text,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
pub(crate) use layout::{Layout, select_sql};

pub(crate) fn decode(row: &Rec, layout: &Layout) -> Result<MssqlIndexOperations> {
    let counters = layout
        .counters()
        .map(|(kind, column)| Ok((kind, optional_integer(row, column)?.map(|value| value.to_string()))))
        .collect::<Result<_>>()?;
    let default =
        optional_text(row, "leading_default")?.as_deref().and_then(culprit_core::sql::mssql_default_generator);
    let value = MssqlIndexOperations {
        identity: MssqlIndexIdentity {
            database_id: integer(row, "database_id")?.try_into().map_err(|_| invalid_data())?,
            object_id: integer(row, "object_id")?.try_into().map_err(|_| invalid_data())?,
            index_id: integer(row, "index_id")?.try_into().map_err(|_| invalid_data())?,
            partition_number: integer(row, "partition_number")?.try_into().map_err(|_| invalid_data())?,
            hobt_id: optional_integer(row, "hobt_id")?.map(|value| value.to_string()),
        },
        schema: optional_text(row, "schema_name")?,
        object: optional_text(row, "object_name")?,
        index: optional_text(row, "index_name")?,
        object_type: optional_text(row, "object_type")?,
        engine_internal: optional_boolean(row, "engine_internal")?,
        leading_key_is_identity: optional_boolean(row, "leading_identity")?,
        leading_key_default: default,
        optimize_for_sequential_key: optional_boolean(row, "optimize_sequential")?,
        counters,
        reset: culprit_core::ResetMarker::Unknown,
    };
    if !value.is_valid() {
        return Err(invalid_data());
    }
    Ok(value)
}

const MAX_APPLICATION_ROWS: usize = 10_000;
async fn application<R: NativeRead>(reader: &mut R) -> Result<projection::Summary> {
    let layout = Layout::read(reader).await?.for_application();
    let rows = reader
        .query(
            &select_sql(&layout),
            vec![Parameter::Integer((MAX_APPLICATION_ROWS + 1) as i64)],
            MAX_APPLICATION_ROWS + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > MAX_APPLICATION_ROWS {
        return Err(CollectionStop::RowLimit.error());
    }
    let decoded = rows.iter().map(|row| decode(row, &layout)).collect::<Result<Vec<_>>>()?;
    projection::summarize(&decoded)
}

pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    snapshot: &mut culprit_core::Snapshot,
) {
    let mut reader = crate::read::ReadSession::application(pool);
    let result = application(&mut reader).await;
    let result = reader.finish(result).await;
    let mut coverage = culprit_core::Coverage::default();
    match result {
        Err(error) => {
            let error = if error.code == culprit_core::ErrorCode::PermissionDenied && error.hint.is_none() {
                error.with_hint(probe.grant_hint())
            } else {
                error
            };
            for source in [&crate::sources::FORWARDED_FETCHES, &crate::sources::INDEX_CONTENTION] {
                coverage.source_failed(source, &error);
            }
        }
        Ok(summary) => {
            for table in &mut snapshot.tables {
                table.forwarded_fetches = summary.forwarded.get(&(table.schema.clone(), table.name.clone())).copied();
            }
            for index in &mut snapshot.indexes {
                index.contention =
                    summary.contention.get(&(index.schema.clone(), index.table.clone(), index.name.clone())).cloned();
            }
            for (source, complete, rows) in [
                (&crate::sources::FORWARDED_FETCHES, summary.forwarded_complete, summary.forwarded.len()),
                (&crate::sources::INDEX_CONTENTION, summary.contention_complete, summary.contention.len()),
            ] {
                coverage.completed(source, rows);
                if !complete {
                    coverage.partial(source, culprit_core::message!("provider.mssql.index_operations.partial"));
                }
            }
        }
    }
    coverage.append_to(snapshot);
}
#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
