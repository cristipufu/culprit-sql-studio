//! Fixture-only current module metadata. Actual XML already comes from the original execution;
//! selecting its cache context/header must not expand every retained plan/type payload in the database.
use crate::{
    cached_statements,
    query_store::payload::Payload,
    read::{NativeRead, Parameter, ReadError, invalid_data, optional_boolean, optional_signed_integer, optional_text},
    relational_inputs::{self, Bindings},
};
use culprit_core::{Result, telemetry::*};
use futures_util::future::LocalBoxFuture;
use serde::Serialize;
use std::collections::BTreeMap;

const SELECTED_ROWS: usize = 2;
const EXACT_SELECTION: &str = "culprit_exact_original";

pub(super) struct StatementMetadata {
    pub context: CachedStatementContext,
    pub module_parameters: MssqlModuleParameters,
    pub statement: String,
}

const ORIGINAL_PREDICATES: &[(&str, &str)] = &[
    ("plan_handle_matches", "qs.plan_handle=CONVERT(varbinary(64),@P3,1)"),
    ("sql_handle_matches", "qs.sql_handle=CONVERT(varbinary(64),@P4,1)"),
    ("statement_start_matches", "qs.statement_start_offset=@P5"),
    ("statement_end_matches", "qs.statement_end_offset=@P6"),
    ("plan_generation_matches", "qs.plan_generation_num=@P7"),
    ("database_matches", "CAST(pa.value AS int)=@P8"),
];

#[derive(Clone, Copy)]
enum Selection<'a> {
    Module,
    Exact(&'a CachedStatementContext),
    Predicates(&'a CachedStatementContext),
}
impl<'a> Selection<'a> {
    fn original(self) -> Option<&'a CachedStatementContext> {
        match self {
            Self::Module => None,
            Self::Exact(original) | Self::Predicates(original) => Some(original),
        }
    }
}

fn query(object: i64, original: Option<&CachedStatementContext>) -> Result<(String, Vec<Parameter>)> {
    selected_query(object, original.map_or(Selection::Module, Selection::Exact))
}

fn selected_query(object: i64, selection: Selection<'_>) -> Result<(String, Vec<Parameter>)> {
    if !(1..=i64::from(i32::MAX)).contains(&object) {
        return Err(invalid_data());
    }
    let mut parameters = vec![Parameter::Integer(SELECTED_ROWS as i64), Parameter::Integer(object)];
    if let Some(original) = selection.original() {
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
    }
    let incarnation = match selection {
        Selection::Exact(_) => format!(
            "AND {}",
            ORIGINAL_PREDICATES.iter().map(|(_, predicate)| *predicate).collect::<Vec<_>>().join(" AND ")
        ),
        _ => String::new(),
    };
    let diagnostic = match selection {
        Selection::Exact(_) => format!(",CAST(1 AS int) AS {EXACT_SELECTION}"),
        Selection::Predicates(_) => ORIGINAL_PREDICATES
            .iter()
            .map(|(name, predicate)| {
                format!(",CASE WHEN {predicate} THEN 1 WHEN NOT({predicate}) THEN 0 END AS culprit_predicate_{name}")
            })
            .collect(),
        Selection::Module => String::new(),
    };
    let selected = format!(
        "SELECT TOP(@P1) {context},{module},st.encrypted{diagnostic} {relation} WHERE {database} \
         AND st.dbid=DB_ID() AND st.objectid=@P2 AND st.number=1 {incarnation}",
        context = cached_statements::context_columns(),
        module = relational_inputs::module_columns("st"),
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE,
    );
    let sql = if matches!(selection, Selection::Exact(_)) {
        parameter_diagnostics(&selected)
    } else {
        format!("{selected} OPTION(MAXDOP 1)")
    };
    Ok((sql, parameters))
}

fn parameter_diagnostics(selected: &str) -> String {
    // Keep the exact native selector intact. OUTER APPLY adds no second cache scan or result set; its one
    // all-NULL selected row is diagnostic only and is refused before any context/header is decoded.
    format!(
        "SELECT selected_cache.*,CASE WHEN @P8=DB_ID() THEN 1 ELSE 0 END AS culprit_bind_database,\
         CASE WHEN LOWER(CONVERT(varchar(130),CONVERT(varbinary(64),@P3,1),1))=LOWER(@P3) THEN 1 ELSE 0 END AS culprit_bind_plan,\
         CASE WHEN LOWER(CONVERT(varchar(130),CONVERT(varbinary(64),@P4,1),1))=LOWER(@P4) THEN 1 ELSE 0 END AS culprit_bind_sql,\
         CASE WHEN @P1=2 AND @P2 BETWEEN 1 AND 2147483647 AND @P7>=0 THEN 1 ELSE 0 END AS culprit_bind_integers,\
         CASE WHEN @P5>=0 AND @P5%2=0 AND (@P6=-1 OR @P6>=@P5 AND @P6%2=0) THEN 1 ELSE 0 END AS culprit_bind_offsets \
         FROM(VALUES(1)) AS diagnostic(only_row) OUTER APPLY({selected}) selected_cache OPTION(MAXDOP 1)"
    )
}

pub(super) async fn read(
    reader: &mut impl NativeRead,
    object: i64,
    original: Option<&CachedStatementContext>,
) -> Result<StatementMetadata> {
    find(reader, object, original).await?.ok_or_else(|| CollectionStop::SourceUnavailable.error())
}

/// A completed empty native selection is distinct from failure. Positive execution callers still use read().
pub(super) async fn find(
    reader: &mut impl NativeRead,
    object: i64,
    original: Option<&CachedStatementContext>,
) -> Result<Option<StatementMetadata>> {
    let (sql, parameters) = query(object, original)?;
    let rows = reader.query(&sql, parameters, SELECTED_ROWS).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else {
        eprintln!(
            "owned scalar metadata refused stage=selected-row; rows={}; expected_original={}; metadata_queries={}",
            rows.len(),
            original.is_some(),
            reader.queries()
        );
        return if rows.is_empty() && original.is_none() {
            Ok(None)
        } else {
            Err(if rows.is_empty() { CollectionStop::SourceUnavailable } else { CollectionStop::InvalidPage }.error())
        };
    };
    if original.is_some() {
        match optional_signed_integer(row, EXACT_SELECTION)? {
            Some(1) => {}
            None => {
                let flags = [
                    "culprit_bind_database",
                    "culprit_bind_plan",
                    "culprit_bind_sql",
                    "culprit_bind_integers",
                    "culprit_bind_offsets",
                ]
                .map(|name| optional_boolean(row, name).map_err(|error| error.code));
                eprintln!(
                    "owned scalar exact selector refused; rows=0; database_matches={:?}; plan_roundtrip={:?}; sql_roundtrip={:?}; integers_valid={:?}; offsets_valid={:?}; metadata_queries={}",
                    flags[0],
                    flags[1],
                    flags[2],
                    flags[3],
                    flags[4],
                    reader.queries()
                );
                // The exact query must return its original DB/binding witness even for no selected row.
                // A different DB or missing/malformed witness is unavailable, never known native absence.
                if flags.iter().any(|flag| !matches!(flag, Ok(Some(true)))) {
                    return Err(CollectionStop::SourceUnavailable.error());
                }
                return Ok(None);
            }
            _ => return Err(invalid_data()),
        }
    }
    let context = cached_statements::context(row)?;
    if let Some(original) = original {
        if original.database_id != context.database_id
            || original.identity.key()? != context.identity.key()?
            || original.query_hash != context.query_hash
        {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    let selected = decode_statement(row, object, context, reader, original.is_some())?;
    // The original creation clock remains part of the full canonical key above. Do not round that native
    // clock to add a SQL comparison between differently represented datetime/datetime2 values.
    let mut binding = Bindings::default();
    if !binding.admit(&selected.context)? {
        return Err(unavailable("binding-admission", reader, original.is_some()));
    }
    binding.require_verified(reader).await?;
    Ok(Some(selected))
}

fn decode_statement(
    row: &crate::value::Rec,
    object: i64,
    context: CachedStatementContext,
    reader: &impl NativeRead,
    expected_original: bool,
) -> Result<StatementMetadata> {
    if optional_signed_integer(row, "module_object_id")? != Some(object) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let module_parameters = relational_inputs::parameters(row, &context)?
        .ok_or_else(|| unavailable("module-header", reader, expected_original))?;
    let Payload::Text(statement) =
        cached_statements::native_statement_from_prefix(&context, optional_text(row, "text_prefix")?.as_deref())
    else {
        return Err(unavailable("statement-slice", reader, expected_original));
    };
    Ok(StatementMetadata { context, module_parameters, statement })
}

/// A later native point is diagnostic only, including when every predicate and the original key match.
/// Its full context contains native identity/clock fields, never the module header or SQL body.
#[derive(Debug, Serialize)]
pub(super) struct NativePredicatePoint {
    context: CachedStatementContext,
    comparison: CacheContextComparison,
    native_predicates: BTreeMap<String, bool>,
    original_binding_verified: Option<bool>,
}

pub(super) fn compare_predicates<'a, R: NativeRead>(
    reader: &'a mut R,
    object: i64,
    original: &'a CachedStatementContext,
) -> LocalBoxFuture<'a, Result<Option<Box<NativePredicatePoint>>>> {
    Box::pin(async move {
        let (sql, parameters) = selected_query(object, Selection::Predicates(original))?;
        let rows = reader.query(&sql, parameters, SELECTED_ROWS).await.map_err(ReadError::into_error)?;
        let row = match rows.as_slice() {
            [] => return Ok(None),
            [row] => row,
            _ => return Err(CollectionStop::InvalidPage.error()),
        };
        let context = cached_statements::context(row)?;
        let selected = decode_statement(row, object, context, reader, true)?;
        let native_predicates = ORIGINAL_PREDICATES
            .iter()
            .map(|(name, _)| {
                let value = optional_boolean(row, &format!("culprit_predicate_{name}"))?.ok_or_else(invalid_data)?;
                Ok(((*name).to_string(), value))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let comparison = CacheContextComparison::between(original, &selected.context);
        let original_binding_verified = if comparison.original_incarnation_matches {
            let mut bindings = Bindings::default();
            if !bindings.admit(original)? {
                return Err(invalid_data());
            }
            Some(bindings.verify(reader).await?.contains(original))
        } else {
            None
        };
        Ok(Some(Box::new(NativePredicatePoint {
            context: selected.context,
            comparison,
            native_predicates,
            original_binding_verified,
        })))
    })
}

fn unavailable(stage: &'static str, reader: &impl NativeRead, expected_original: bool) -> culprit_core::Error {
    // Only fixed refusal stages and existing dispatch accounting. Never retain SQL, handles or parameter values.
    eprintln!(
        "owned scalar metadata refused stage={stage}; expected_original={expected_original}; metadata_queries={}",
        reader.queries()
    );
    CollectionStop::SourceUnavailable.error()
}

#[cfg(test)]
pub(super) mod tests;

/// Only equality flags leave this fixture diagnostic. Native values/handles and replacement contexts do not.
#[derive(Debug, serde::Serialize)]
pub(super) struct CacheContextComparison {
    database_matches: bool,
    plan_handle_matches: bool,
    sql_handle_matches: bool,
    statement_start_matches: bool,
    statement_end_matches: bool,
    plan_generation_matches: bool,
    creation_time_matches: bool,
    query_hash_matches: bool,
    original_incarnation_matches: bool,
}
impl CacheContextComparison {
    pub(super) fn require_original_match(self) -> Result<()> {
        if self.original_incarnation_matches { Ok(()) } else { Err(CollectionStop::SourceChanged.error()) }
    }

    pub(super) fn between(original: &CachedStatementContext, current: &CachedStatementContext) -> Self {
        let left = &original.identity;
        let right = &current.identity;
        Self {
            database_matches: original.database_id == current.database_id,
            plan_handle_matches: left.plan_handle == right.plan_handle,
            sql_handle_matches: left.sql_handle == right.sql_handle,
            statement_start_matches: left.statement_start_bytes == right.statement_start_bytes,
            statement_end_matches: left.statement_end_bytes == right.statement_end_bytes,
            plan_generation_matches: left.plan_generation == right.plan_generation,
            creation_time_matches: left.created_without_offset == right.created_without_offset,
            query_hash_matches: original.query_hash == current.query_hash,
            original_incarnation_matches: original.database_id == current.database_id
                && left == right
                && original.query_hash == current.query_hash,
        }
    }
}
