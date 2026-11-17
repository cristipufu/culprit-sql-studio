//! One original Query Store definition index; source receipts and logical namespace are independent of time windows.
use culprit_core::{Error, Result, query_store_statement_report::*, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

// The existing bounded referenced-definition population; overflow never selects a shortened roster.
const MAX_DEFINITION_RECORDS: usize = 10_000;
const STATEMENT_PREVIEW_CHARACTERS: usize = 512;

pub struct QueryStoreDefinitionRecord<'a> {
    pub reference: DiagnosticEvidenceRef,
    pub definition: &'a QueryDefinition,
}

#[derive(Clone, Copy)]
pub struct QueryStoreDefinitionEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub rows: &'a [QueryStoreDefinitionRecord<'a>],
    pub selected_rows: u32,
    pub read_complete: bool,
}

/// These are borrowed original carriers. A history interval never becomes a native acquisition clock.
pub(crate) enum QueryStoreStatementScope<'a> {
    NativeRead(Option<&'a NativeReadContext>),
    History(&'a CollectionDescription),
}
impl QueryStoreStatementScope<'_> {
    fn namespace(&self) -> Result<Option<String>> {
        match self {
            Self::NativeRead(None) => Ok(None),
            Self::NativeRead(Some(context)) => {
                if !context.is_valid() {
                    return Err(invalid());
                }
                namespace(&context.target, &context.partition)
            }
            Self::History(source) => {
                validate_history(source)?;
                namespace(&source.target, &source.partition)
            }
        }
    }
}

pub(crate) struct QueryStoreDefinitions<'a> {
    input: QueryStoreDefinitionEvidence<'a>,
    namespace: QueryStoreDefinitionNamespace,
    rows: BTreeMap<String, &'a QueryStoreDefinitionRecord<'a>>,
}
impl<'a> QueryStoreDefinitions<'a> {
    pub(crate) fn new(original: QueryStoreStatementScope<'_>, input: QueryStoreDefinitionEvidence<'a>) -> Result<Self> {
        validate_history(input.source)?;
        if input.rows.len() > MAX_DEFINITION_RECORDS
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
            if row.reference.stream != TelemetryStream::QueryDefinitions
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
            let key = query_store_query_incarnation_key(&row.definition.identity).map_err(|_| invalid())?;
            if rows.insert(key, row).is_some() {
                return Err(invalid());
            }
        }
        let namespace = match (original.namespace()?, namespace(&input.source.target, &input.source.partition)?) {
            (Some(left), Some(right)) if left == right => QueryStoreDefinitionNamespace::Matched,
            (Some(_), Some(_)) => QueryStoreDefinitionNamespace::Changed,
            _ => QueryStoreDefinitionNamespace::Unverified,
        };
        Ok(Self { input, namespace, rows })
    }

    pub(crate) fn report(&self) -> QueryStoreDefinitionRead {
        QueryStoreDefinitionRead {
            namespace: self.namespace,
            selected_rows: self.input.selected_rows,
            observed_rows: self.input.rows.len() as u32,
            read_complete: self.input.read_complete,
        }
    }

    pub(crate) fn resolve(&self, identity: Option<&NativeQueryIdentity>) -> QueryStoreDefinitionBinding<'a> {
        let Some(identity) = identity.filter(|identity| query_store_query_identity_is_complete(identity)) else {
            return QueryStoreDefinitionBinding::IdentityUnavailable;
        };
        match self.namespace {
            QueryStoreDefinitionNamespace::Unverified => return QueryStoreDefinitionBinding::SourceUnverified,
            QueryStoreDefinitionNamespace::Changed => return QueryStoreDefinitionBinding::SourceChanged,
            QueryStoreDefinitionNamespace::Matched => (),
        }
        let Ok(key) = query_store_query_incarnation_key(identity) else {
            return QueryStoreDefinitionBinding::IdentityUnavailable;
        };
        match self.rows.get(&key) {
            Some(row) => QueryStoreDefinitionBinding::Matched(row),
            None => QueryStoreDefinitionBinding::NotObserved,
        }
    }
}

pub(crate) enum QueryStoreDefinitionBinding<'a> {
    Matched(&'a QueryStoreDefinitionRecord<'a>),
    NotObserved,
    SourceUnverified,
    SourceChanged,
    IdentityUnavailable,
}
impl QueryStoreDefinitionBinding<'_> {
    pub(crate) fn state(&self) -> QueryStoreStatementBinding {
        match self {
            Self::Matched(_) => QueryStoreStatementBinding::Matched,
            Self::NotObserved => QueryStoreStatementBinding::NotObserved,
            Self::SourceUnverified => QueryStoreStatementBinding::SourceUnverified,
            Self::SourceChanged => QueryStoreStatementBinding::SourceChanged,
            Self::IdentityUnavailable => QueryStoreStatementBinding::IdentityUnavailable,
        }
    }
}

/// Domain renderers supply their own prose. The complete verified payload and one preview policy stay shared.
pub(crate) struct StatementEvidence<'a> {
    pub binding: QueryStoreStatementBinding,
    pub payload: &'a DefinitionPayload,
    pub origin: StatementOrigin,
    pub monitoring_template_id: Option<String>,
    pub definition: Option<DiagnosticEvidenceRef>,
}
impl StatementEvidence<'_> {
    pub(crate) fn explanation(self, explanation: String) -> QueryStoreStatementExplanation {
        let (preview, preview_truncated, unavailable) = match self.payload {
            DefinitionPayload::Available { text, .. } => (
                Some(text.chars().take(STATEMENT_PREVIEW_CHARACTERS).collect()),
                text.chars().count() > STATEMENT_PREVIEW_CHARACTERS,
                None,
            ),
            DefinitionPayload::Unavailable { reason } => (None, false, Some(*reason)),
        };
        QueryStoreStatementExplanation {
            binding: self.binding,
            definition: self.definition,
            origin: self.origin,
            monitoring_template_id: self.monitoring_template_id,
            preview,
            preview_truncated,
            unavailable,
            unavailable_explanation: unavailable.map(payload_unavailable),
            explanation,
        }
    }
}

fn namespace(target: &CollectionTarget, partition: &SourcePartition) -> Result<Option<String>> {
    if target.engine() != culprit_core::Engine::Mssql
        || target.database().is_none()
        || *partition != SourcePartition::Target
    {
        return Err(invalid());
    }
    Ok(query_store_database_namespace(target, partition))
}
fn validate_history(source: &CollectionDescription) -> Result<()> {
    source.captured_range().map_err(|_| invalid())?;
    if !source.is_partition_qualified() || source.query_history().is_none_or(|history| !history.is_valid()) {
        return Err(invalid());
    }
    namespace(&source.target, &source.partition)?;
    Ok(())
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

/// This checks internal native-reference consistency after the caller validated the original runtime receipt.
/// It does not authenticate a source or synthesize an identity from SQL, hashes or a plan number.
pub(crate) fn runtime_query_identity(
    source: &CollectionDescription,
    key: &NativeMeasurementKey,
) -> Result<Option<NativeQueryIdentity>> {
    if let Some(plan) = &key.references.plan
        && key.native.qualifiers.get("plan_id").is_some_and(|actual| actual != &plan.native.id)
    {
        return Err(invalid());
    }
    let Some(reference) = &key.references.query else { return Ok(None) };
    let identity =
        NativeQueryIdentity { query_id: reference.native.id.clone(), qualifiers: reference.native.qualifiers.clone() };
    if reference
        .definition_id
        .as_ref()
        .is_some_and(|actual| !identity.definition_id(source).is_ok_and(|expected| &expected == actual))
    {
        return Err(invalid());
    }
    if let Some(plan) = &key.references.plan {
        let original = NativePlanIdentity {
            plan_id: plan.native.id.clone(),
            query: identity.clone(),
            qualifiers: plan.native.qualifiers.clone(),
        };
        if plan
            .definition_id
            .as_ref()
            .is_some_and(|actual| !original.definition_id(source).is_ok_and(|expected| &expected == actual))
        {
            return Err(invalid());
        }
    }
    Ok(query_store_query_identity_is_complete(&identity).then_some(identity))
}

#[cfg(test)]
mod tests;

fn payload_unavailable(reason: PayloadUnavailable) -> String {
    let reason = match reason {
        PayloadUnavailable::Missing => culprit_core::t!("engine.cached_plans.reason.missing"),
        PayloadUnavailable::Restricted => culprit_core::t!("engine.cached_plans.reason.restricted"),
        PayloadUnavailable::Encrypted => culprit_core::t!("engine.cached_plans.reason.encrypted"),
        PayloadUnavailable::Oversized => culprit_core::t!("engine.cached_plans.reason.oversized"),
        PayloadUnavailable::RedactionFailed => culprit_core::t!("engine.cached_plans.reason.redaction"),
        PayloadUnavailable::NotCollected => culprit_core::t!("engine.cached_plans.reason.not_collected"),
        PayloadUnavailable::SourceUnavailable => culprit_core::t!("engine.cached_plans.reason.source"),
    };
    culprit_core::t!("view.query_store_statement.unavailable", reason = reason)
}
