//! Compilation policy uses the shared exact Query Store binder; compiler wording stays local.
use super::*;
use crate::views::query_store_definitions::{
    QueryStoreDefinitionBinding, QueryStoreDefinitions, QueryStoreStatementScope, StatementEvidence,
};
use culprit_core::{query_store_statement_report::*, telemetry::*};

pub(super) struct Definitions<'a> {
    index: QueryStoreDefinitions<'a>,
}
impl<'a> Definitions<'a> {
    pub(super) fn new(context: Option<&NativeReadContext>, input: QueryStoreDefinitionEvidence<'a>) -> Result<Self> {
        Ok(Self { index: QueryStoreDefinitions::new(QueryStoreStatementScope::NativeRead(context), input)? })
    }
    pub(super) fn report(&self) -> QueryStoreDefinitionRead {
        self.index.report()
    }
    pub(super) fn append_limitations(&self, limitations: &mut Vec<String>) {
        limitations.push(t!("view.compilation.statement.limit.selection"));
        limitations.push(t!("view.compilation.statement.limit.continuity"));
        let read = self.index.report();
        match read.namespace {
            QueryStoreDefinitionNamespace::Matched => (),
            QueryStoreDefinitionNamespace::Unverified => {
                limitations.push(t!("view.compilation.statement.limit.namespace"))
            }
            QueryStoreDefinitionNamespace::Changed => limitations.push(t!("view.compilation.statement.limit.changed")),
        }
        if !read.read_complete {
            limitations.push(t!("view.compilation.statement.limit.partial"));
        }
    }
    pub(super) fn resolve(&self, native: &'a MssqlQueryCompilation) -> Result<Statement<'a>> {
        let binding = self.index.resolve(Some(&native.identity));
        let QueryStoreDefinitionBinding::Matched(row) = binding else {
            return Ok(Statement::without_definition(native, binding.state()));
        };
        let definition = row.definition;
        if matches!((&native.statement,&definition.sql),
            (DefinitionPayload::Available {sha256:left,..},DefinitionPayload::Available {sha256:right,..}) if left!=right)
            || native.origin != StatementOrigin::Unknown
                && definition.origin != StatementOrigin::Unknown
                && native.origin != definition.origin
        {
            return Err(invalid());
        }
        if native.statement.is_available() {
            return Ok(Statement::captured(native));
        }
        Ok(Statement {
            payload: &definition.sql,
            origin: definition.origin,
            report: Some(Box::new(statement_report(
                QueryStoreStatementBinding::Matched,
                &definition.sql,
                definition.origin,
                definition.monitoring_template_id.clone(),
                Some(row.reference.clone()),
            ))),
        })
    }
}

pub(super) struct Statement<'a> {
    pub(super) payload: &'a DefinitionPayload,
    pub(super) origin: StatementOrigin,
    pub(super) report: Option<Box<QueryStoreStatementExplanation>>,
}
impl<'a> Statement<'a> {
    pub(super) fn captured(native: &'a MssqlQueryCompilation) -> Self {
        let recorded =
            !matches!(native.statement, DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
        Self {
            payload: &native.statement,
            origin: native.origin,
            report: recorded.then(|| {
                Box::new(statement_report(
                    QueryStoreStatementBinding::Captured,
                    &native.statement,
                    native.origin,
                    None,
                    None,
                ))
            }),
        }
    }
    fn without_definition(native: &'a MssqlQueryCompilation, binding: QueryStoreStatementBinding) -> Self {
        if native.statement.is_available() {
            return Self::captured(native);
        }
        Self {
            payload: &native.statement,
            origin: native.origin,
            report: Some(Box::new(statement_report(binding, &native.statement, native.origin, None, None))),
        }
    }
}

fn statement_report(
    binding: QueryStoreStatementBinding,
    payload: &DefinitionPayload,
    origin: StatementOrigin,
    monitoring_template_id: Option<String>,
    definition: Option<DiagnosticEvidenceRef>,
) -> QueryStoreStatementExplanation {
    let binding_explanation = match binding {
        QueryStoreStatementBinding::Captured => t!("view.compilation.statement.captured"),
        QueryStoreStatementBinding::Matched => t!("view.compilation.statement.matched"),
        QueryStoreStatementBinding::IdentityUnavailable | QueryStoreStatementBinding::NotObserved => {
            t!("view.compilation.statement.not_observed")
        }
        QueryStoreStatementBinding::SourceUnverified => t!("view.compilation.statement.unverified"),
        QueryStoreStatementBinding::SourceChanged => t!("view.compilation.statement.changed"),
    };
    let provenance = match origin {
        StatementOrigin::Application => t!("view.compilation.statement.origin.application"),
        StatementOrigin::CulpritMonitoring | StatementOrigin::Culprit => {
            t!("view.compilation.statement.origin.culprit")
        }
        StatementOrigin::EngineInternal => t!("view.compilation.statement.origin.internal"),
        StatementOrigin::Unknown => t!("view.compilation.statement.origin.unknown"),
    };

    StatementEvidence { binding, payload, origin, monitoring_template_id, definition }
        .explanation(format!("{binding_explanation} {provenance}"))
}

#[cfg(test)]
mod tests;
