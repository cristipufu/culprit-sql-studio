//! A definition can supply policy only through its original Query Store namespace and exact native incarnation.
use super::*;
use culprit_core::{
    compilation_report::{
        CompilationDefinitionNamespace, CompilationDefinitionRead, CompilationStatementBinding,
        CompilationStatementExplanation,
    },
    telemetry::*,
};
use std::collections::BTreeMap;

const STATEMENT_PREVIEW_CHARACTERS: usize = 512;

pub struct CompilationDefinitionRecord<'a> {
    pub reference: DiagnosticEvidenceRef,
    pub definition: &'a QueryDefinition,
}

#[derive(Clone, Copy)]
pub struct CompilationDefinitionEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub rows: &'a [CompilationDefinitionRecord<'a>],
    pub selected_rows: u32,
    pub read_complete: bool,
}

pub(super) struct Definitions<'a> {
    input: CompilationDefinitionEvidence<'a>,
    namespace: CompilationDefinitionNamespace,
    rows: BTreeMap<String, &'a CompilationDefinitionRecord<'a>>,
}

impl<'a> Definitions<'a> {
    pub(super) fn new(context: Option<&NativeReadContext>, input: CompilationDefinitionEvidence<'a>) -> Result<Self> {
        let history = input.source.query_history().ok_or_else(invalid)?;
        if !input.source.is_partition_qualified()
            || input.source.target.engine() != culprit_core::Engine::Mssql
            || input.source.target.database().is_none()
            || !history.is_valid()
            || ObservationBounds::new(&history.interval.from, &history.interval.until).is_none()
            || input.rows.len() > MAX_QUERY_COMPILATION_OBSERVATIONS
            || input.rows.len() > input.selected_rows as usize
            || input.read_complete && input.rows.len() != input.selected_rows as usize
        {
            return Err(invalid());
        }
        let mut rows = BTreeMap::new();
        let mut records = BTreeSet::new();
        let mut ordinals = BTreeSet::new();
        let mut capture = None;
        for row in input.rows {
            let native = TelemetryRecord::QueryDefinition(row.definition.clone());
            if !row.reference.is_valid()
                || row.reference.stream != TelemetryStream::QueryDefinitions
                || row.reference.row_ordinal >= input.selected_rows
                || !native.is_valid()
                || row.definition.identity.definition_id(input.source).map_err(|_| invalid())? != row.definition.id
            {
                return Err(invalid());
            }
            row.reference.validate_canonical_record(input.source, &native).map_err(|_| invalid())?;
            let original_capture = row.reference.capture_ref();
            if capture.as_ref().is_some_and(|capture| capture != &original_capture)
                || !records.insert(&row.reference.record_id)
                || !ordinals.insert(row.reference.row_ordinal)
            {
                return Err(invalid());
            }
            capture = Some(original_capture);
            let key = MssqlQueryCompilation::identity_key(&row.definition.identity).map_err(|_| invalid())?;
            if rows.insert(key, row).is_some() {
                return Err(invalid());
            }
        }
        let namespace = match context {
            Some(context)
                if !context.is_valid()
                    || context.target.engine() != culprit_core::Engine::Mssql
                    || context.target.database().is_none() =>
            {
                return Err(invalid());
            }
            Some(context) => match (
                compilation_namespace(context),
                compilation_database_namespace(&input.source.target, &input.source.partition),
            ) {
                (Some(left), Some(right)) if left == right => CompilationDefinitionNamespace::Matched,
                (Some(_), Some(_)) => CompilationDefinitionNamespace::Changed,
                _ => CompilationDefinitionNamespace::Unverified,
            },
            None => CompilationDefinitionNamespace::Unverified,
        };
        Ok(Self { input, namespace, rows })
    }

    pub(super) fn report(&self) -> CompilationDefinitionRead {
        CompilationDefinitionRead {
            namespace: self.namespace,
            selected_rows: self.input.selected_rows,
            observed_rows: self.input.rows.len() as u32,
            read_complete: self.input.read_complete,
        }
    }

    pub(super) fn append_limitations(&self, limitations: &mut Vec<String>) {
        limitations.push(t!("view.compilation.statement.limit.selection"));
        limitations.push(t!("view.compilation.statement.limit.continuity"));
        match self.namespace {
            CompilationDefinitionNamespace::Matched => {}
            CompilationDefinitionNamespace::Unverified => {
                limitations.push(t!("view.compilation.statement.limit.namespace"));
            }
            CompilationDefinitionNamespace::Changed => {
                limitations.push(t!("view.compilation.statement.limit.changed"));
            }
        }
        if !self.input.read_complete {
            limitations.push(t!("view.compilation.statement.limit.partial"));
        }
    }

    pub(super) fn resolve(&self, native: &'a MssqlQueryCompilation) -> Result<Statement<'a>> {
        let binding = match self.namespace {
            CompilationDefinitionNamespace::Matched => CompilationStatementBinding::NotObserved,
            CompilationDefinitionNamespace::Unverified => CompilationStatementBinding::SourceUnverified,
            CompilationDefinitionNamespace::Changed => CompilationStatementBinding::SourceChanged,
        };
        if self.namespace != CompilationDefinitionNamespace::Matched {
            return Ok(Statement::without_definition(native, binding));
        }
        let key = MssqlQueryCompilation::identity_key(&native.identity).map_err(|_| invalid())?;
        let Some(row) = self.rows.get(&key) else {
            return Ok(Statement::without_definition(native, binding));
        };
        let definition = row.definition;
        if matches!(
            (&native.statement, &definition.sql),
            (DefinitionPayload::Available { sha256: left, .. }, DefinitionPayload::Available { sha256: right, .. })
                if left != right
        ) || native.origin != StatementOrigin::Unknown
            && definition.origin != StatementOrigin::Unknown
            && native.origin != definition.origin
        {
            // Two authenticated original statements cannot select a representative for one native incarnation.
            return Err(invalid());
        }
        if native.statement.is_available() {
            return Ok(Statement::captured(native));
        }
        Ok(Statement {
            payload: &definition.sql,
            origin: definition.origin,
            report: Some(Box::new(statement_report(
                CompilationStatementBinding::Matched,
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
    pub(super) report: Option<Box<CompilationStatementExplanation>>,
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
                    CompilationStatementBinding::Captured,
                    &native.statement,
                    native.origin,
                    None,
                    None,
                ))
            }),
        }
    }

    fn without_definition(native: &'a MssqlQueryCompilation, binding: CompilationStatementBinding) -> Self {
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
    binding: CompilationStatementBinding,
    payload: &DefinitionPayload,
    origin: StatementOrigin,
    monitoring_template_id: Option<String>,
    definition: Option<DiagnosticEvidenceRef>,
) -> CompilationStatementExplanation {
    let (preview, preview_truncated, unavailable) = match payload {
        DefinitionPayload::Available { text, .. } => (
            Some(text.chars().take(STATEMENT_PREVIEW_CHARACTERS).collect()),
            text.chars().count() > STATEMENT_PREVIEW_CHARACTERS,
            None,
        ),
        DefinitionPayload::Unavailable { reason } => (None, false, Some(*reason)),
    };
    let binding_explanation = match binding {
        CompilationStatementBinding::Captured => t!("view.compilation.statement.captured"),
        CompilationStatementBinding::Matched => t!("view.compilation.statement.matched"),
        CompilationStatementBinding::NotObserved => t!("view.compilation.statement.not_observed"),
        CompilationStatementBinding::SourceUnverified => t!("view.compilation.statement.unverified"),
        CompilationStatementBinding::SourceChanged => t!("view.compilation.statement.changed"),
    };
    let provenance = match origin {
        StatementOrigin::Application => t!("view.compilation.statement.origin.application"),
        StatementOrigin::CulpritMonitoring | StatementOrigin::Culprit => {
            t!("view.compilation.statement.origin.culprit")
        }
        StatementOrigin::EngineInternal => t!("view.compilation.statement.origin.internal"),
        StatementOrigin::Unknown => t!("view.compilation.statement.origin.unknown"),
    };
    CompilationStatementExplanation {
        binding,
        definition,
        origin,
        monitoring_template_id,
        preview,
        preview_truncated,
        unavailable,
        explanation: format!("{binding_explanation} {provenance}"),
    }
}

#[cfg(test)]
mod tests;
