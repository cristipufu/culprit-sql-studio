//! Original SQL-handle module declarations and an independent native plan-to-SQL-token check. Plan handles alone
//! can be reused after ALTER/eviction; neither a plan handle nor identical statement text proves incarnation.

#[cfg(test)]
mod native_tests;
mod sql;
#[cfg(test)]
pub(crate) mod tests;

use crate::{
    read::{NativeRead, Parameter, ReadError, invalid_data, optional_signed_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{CachedStatementContext, CollectionStop, MssqlModuleParameters},
};
use std::collections::{BTreeMap, BTreeSet};

/// A bounded profile, not a claim that a returned-row limit alone bounds native work. Matches the default snapshot
/// row ceiling; higher requested captures keep unadmitted plan/type enrichment explicitly unavailable.
pub(crate) const MAX_BINDINGS: usize = 500;

/// Native text is already selected from qs.sql_handle by both existing payload readers. Reuse its bounded prefix,
/// source length and module identifiers; never read today's sys.parameters or serialize the SQL header/defaults.
pub(crate) fn module_columns(text_alias: &str) -> String {
    format!(
        "{text_alias}.dbid AS module_database_id,{text_alias}.objectid AS module_object_id,{text_alias}.number AS module_number"
    )
}

pub(crate) fn parameters(row: &Rec, context: &CachedStatementContext) -> Result<Option<MssqlModuleParameters>> {
    if crate::read::optional_boolean(row, "encrypted")? != Some(false) {
        return Ok(None);
    }
    let (Some(database), Some(object), Some(number), Some(prefix)) = (
        optional_signed_integer(row, "module_database_id")?,
        optional_signed_integer(row, "module_object_id")?,
        optional_signed_integer(row, "module_number")?,
        optional_text(row, "text_prefix")?,
    ) else {
        return Ok(None);
    };
    if database != i64::from(context.database_id) || object <= 0 || number != 1 {
        return Ok(None);
    }
    if prefix.chars().count() > crate::statement_origin::PROVENANCE_PREFIX_CHARACTERS {
        return Err(invalid_data());
    }
    let Some(source_bytes) = context.source_text_bytes.as_deref().and_then(|bytes| bytes.parse::<u64>().ok()) else {
        return Ok(None);
    };
    let prefix_bytes = prefix.encode_utf16().count() as u64 * 2;
    if prefix_bytes > source_bytes || context.source_text_complete && prefix_bytes != source_bytes {
        return Err(invalid_data());
    }
    let Some(table_parameters) = culprit_core::sql::mssql_procedure_table_parameters(&prefix) else { return Ok(None) };
    let value = MssqlModuleParameters {
        database_id: database.try_into().map_err(|_| invalid_data())?,
        object_id: object.try_into().map_err(|_| invalid_data())?,
        module_number: 1,
        sql_handle: context.identity.sql_handle.clone(),
        table_parameters,
    };
    Ok(value.is_valid_for(context).then_some(value))
}

#[derive(Default)]
pub(crate) struct Bindings {
    pairs: BTreeMap<String, String>,
    admitted: BTreeSet<String>,
}
impl Bindings {
    pub(crate) fn observe(&mut self, rows: &[Rec]) -> Result<()> {
        for row in rows {
            self.admit(&crate::cached_statements::context(row)?)?;
        }
        Ok(())
    }
    pub(crate) fn admit(&mut self, context: &CachedStatementContext) -> Result<bool> {
        if !context.is_valid() || !context.identity.has_plan_handle() || !context.identity.has_sql_handle() {
            return Ok(false);
        }
        let plan = context.identity.plan_handle.to_ascii_lowercase();
        let sql = context.identity.sql_handle.to_ascii_lowercase();
        let key = context.identity.key()?;
        if let Some(previous) = self.pairs.get(&plan)
            && previous != &sql
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        if self.admitted.contains(&key) {
            return Ok(true);
        }
        if self.admitted.len() >= MAX_BINDINGS {
            return Ok(false);
        }
        self.pairs.insert(plan, sql);
        self.admitted.insert(key);
        Ok(true)
    }
    pub(crate) fn contains(&self, context: &CachedStatementContext) -> bool {
        context.identity.key().is_ok_and(|key| self.admitted.contains(&key))
    }
    pub(crate) async fn verify<R: NativeRead>(&self, reader: &mut R) -> Result<VerifiedBindings> {
        if self.pairs.is_empty() {
            return Ok(VerifiedBindings::default());
        }
        let rows = reader
            .query(sql::VERIFY, vec![self.parameter()], self.pairs.len() + 1)
            .await
            .map_err(ReadError::into_error)?;
        self.accept(rows)
    }
    fn parameter(&self) -> Parameter {
        let mut selectors = String::from("<plans>");
        for plan in self.pairs.keys() {
            selectors.push_str(&format!("<plan handle=\"{plan}\"/>"));
        }
        selectors.push_str("</plans>");
        Parameter::Text(selectors)
    }
    fn accept(&self, rows: Vec<Rec>) -> Result<VerifiedBindings> {
        if rows.len() > self.pairs.len() {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut seen = BTreeSet::new();
        let mut verified = VerifiedBindings::default();
        for row in rows {
            let plan = text(&row, "plan_handle")?.to_ascii_lowercase();
            let Some(expected) = self.pairs.get(&plan) else { return Err(invalid_data()) };
            if !seen.insert(plan.clone()) {
                return Err(invalid_data());
            }
            if optional_text(&row, "sql_handle")?.is_some_and(|sql| sql.eq_ignore_ascii_case(expected)) {
                verified.pairs.insert(plan, expected.clone());
            }
        }
        if seen.len() != self.pairs.len() {
            return Err(invalid_data());
        }
        Ok(verified)
    }
    pub(crate) fn all_verified(&self, verified: &VerifiedBindings) -> bool {
        self.pairs == verified.pairs
    }
    pub(crate) async fn require_verified<R: NativeRead>(&self, reader: &mut R) -> Result<()> {
        let verified = self.verify(reader).await?;
        if !self.all_verified(&verified) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct VerifiedBindings {
    pairs: BTreeMap<String, String>,
}
impl VerifiedBindings {
    pub(crate) fn contains(&self, context: &CachedStatementContext) -> bool {
        self.pairs
            .get(&context.identity.plan_handle.to_ascii_lowercase())
            .is_some_and(|sql| sql.eq_ignore_ascii_case(&context.identity.sql_handle))
    }
}
