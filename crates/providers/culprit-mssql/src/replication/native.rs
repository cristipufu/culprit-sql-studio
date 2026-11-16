//! Expected view-availability failures finish on the original native owner; other failures retire it.
use super::*;
use culprit_core::{Error, ErrorCode};

pub(super) const AVAILABILITY_ERROR: &str = "replication_source_error";
// https://learn.microsoft.com/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999
// 208 is an unavailable object/interface; 229,297,300 are permission failures. A missing column, procedure,
// timeout or other native fault is deliberately outside this source-result contract.
const PERMISSION_ERRORS: &[i64] = &[229, 297, 300];
const INTERFACE_ERRORS: &[i64] = &[208];

pub(super) enum NativeRows {
    Available(Vec<Rec>),
    Unavailable(Error),
}

pub(super) async fn read<R: NativeRead>(reader: &mut R, sql: &str, limit: usize) -> Result<NativeRows> {
    let rows = reader.query(&guarded_sql(sql), vec![], limit).await.map_err(ReadError::into_error)?;
    availability(rows)
}

fn guarded_sql(sql: &str) -> String {
    // Only this module's fixed metadata SELECTs enter this boundary. The nested native statement carries its
    // own monitoring marker because it can be recorded independently from the outer batch.
    let sql = crate::statement_origin::monitoring_batch(sql).replace('\'', "''");
    let expected = PERMISSION_ERRORS.iter().chain(INTERFACE_ERRORS).map(i64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "BEGIN TRY\n EXEC sys.sp_executesql N'{sql}';\nEND TRY\nBEGIN CATCH\n IF ERROR_NUMBER() NOT IN({expected}) THROW;\n SELECT CAST(ERROR_NUMBER() AS bigint) AS {AVAILABILITY_ERROR};\nEND CATCH"
    )
}

fn availability(rows: Vec<Rec>) -> Result<NativeRows> {
    if !rows.iter().any(|row| row.columns().any(|(name, _)| name == AVAILABILITY_ERROR)) {
        return Ok(NativeRows::Available(rows));
    }
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    if row.columns().count() != 1 {
        return Err(invalid_data());
    }
    let number = integer(row, AVAILABILITY_ERROR)?;
    let code = if PERMISSION_ERRORS.contains(&number) {
        ErrorCode::PermissionDenied
    } else if INTERFACE_ERRORS.contains(&number) {
        // Error 208 can describe a missing or metadata-hidden object. It is unavailable on this connection,
        // never proof that replication is absent or unsupported by the product.
        ErrorCode::Unsupported
    } else {
        return Err(invalid_data());
    };
    Ok(NativeRows::Unavailable(Error::new(code, culprit_core::t!("provider.mssql.replication.native_unavailable"))))
}

#[cfg(test)]
mod tests;
