//! Fixture-only current module metadata. Actual XML already comes from the original execution;
//! selecting its cache context/header must not expand every retained plan/type payload in the database.
use crate::{
    cached_statements,
    query_store::payload::Payload,
    read::{NativeRead, Parameter, ReadError, invalid_data, optional_signed_integer, optional_text},
    relational_inputs::{self, Bindings},
};
use culprit_core::{Result, telemetry::*};

const SELECTED_ROWS: usize = 2;

pub(super) struct StatementMetadata {
    pub context: CachedStatementContext,
    pub module_parameters: MssqlModuleParameters,
    pub statement: String,
}

fn query(object: i64, original: Option<&CachedStatementContext>) -> Result<(String, Vec<Parameter>)> {
    if !(1..=i64::from(i32::MAX)).contains(&object) {
        return Err(invalid_data());
    }
    let mut parameters = vec![Parameter::Integer(SELECTED_ROWS as i64), Parameter::Integer(object)];
    let incarnation = if let Some(original) = original {
        if !original.is_valid() || !original.identity.has_plan_handle() || !original.identity.has_sql_handle() {
            return Err(invalid_data());
        }
        let identity = &original.identity;
        parameters.extend([
            Parameter::Text(identity.plan_handle.clone()),
            Parameter::Text(identity.sql_handle.clone()),
            Parameter::Integer(i64::from(identity.statement_start_bytes)),
            Parameter::Integer(i64::from(identity.statement_end_bytes)),
            Parameter::Integer(identity.plan_generation.parse().map_err(|_| invalid_data())?),
            Parameter::Integer(i64::from(original.database_id)),
        ]);
        "AND qs.plan_handle=CONVERT(varbinary(64),@P3,1) AND qs.sql_handle=CONVERT(varbinary(64),@P4,1) \
         AND qs.statement_start_offset=@P5 AND qs.statement_end_offset=@P6 AND qs.plan_generation_num=@P7 \
         AND CAST(pa.value AS int)=@P8"
    } else {
        ""
    };
    Ok((
        format!(
            "SELECT TOP(@P1) {context},{module},st.encrypted {relation} WHERE {database} \
             AND st.dbid=DB_ID() AND st.objectid=@P2 AND st.number=1 {incarnation} OPTION(MAXDOP 1)",
            context = cached_statements::context_columns(),
            module = relational_inputs::module_columns("st"),
            relation = cached_statements::relation(),
            database = cached_statements::DATABASE_PREDICATE,
        ),
        parameters,
    ))
}

pub(super) async fn read(
    reader: &mut impl NativeRead,
    object: i64,
    original: Option<&CachedStatementContext>,
) -> Result<StatementMetadata> {
    let (sql, parameters) = query(object, original)?;
    let rows = reader.query(&sql, parameters, SELECTED_ROWS).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        return Err(
            if rows.is_empty() { CollectionStop::SourceUnavailable } else { CollectionStop::InvalidPage }.error()
        );
    };
    let context = cached_statements::context(row)?;
    if let Some(original) = original {
        if original.database_id != context.database_id
            || original.identity.key()? != context.identity.key()?
            || original.query_hash != context.query_hash
        {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    if optional_signed_integer(row, "module_object_id")? != Some(object) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let module_parameters =
        relational_inputs::parameters(row, &context)?.ok_or_else(|| CollectionStop::SourceUnavailable.error())?;
    let Payload::Text(statement) =
        cached_statements::native_statement_from_prefix(&context, optional_text(row, "text_prefix")?.as_deref())
    else {
        return Err(CollectionStop::SourceUnavailable.error());
    };
    // The original creation clock remains part of the full canonical key above. Do not round that native
    // clock to add a SQL comparison between differently represented datetime/datetime2 values.
    let mut binding = Bindings::default();
    if !binding.admit(&context)? {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    binding.require_verified(reader).await?;
    Ok(StatementMetadata { context, module_parameters, statement })
}

#[cfg(test)]
mod tests;
