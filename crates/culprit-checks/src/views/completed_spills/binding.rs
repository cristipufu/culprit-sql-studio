use super::*;

const STATEMENT_PREVIEW_CHARACTERS: usize = 512;
type DefinitionIndex<'a> = BTreeMap<(u32, String), (&'a MssqlCachedPayload, &'a DiagnosticEvidenceRef)>;

pub(super) struct Binding {
    pub state: CompletedSpillStatementBinding,
    pub reference: Option<DiagnosticEvidenceRef>,
    pub preview: Option<String>,
    pub truncated: bool,
    pub unavailable: Option<PayloadUnavailable>,
}
impl Binding {
    fn unavailable(state: CompletedSpillStatementBinding) -> Self {
        Self { state, reference: None, preview: None, truncated: false, unavailable: None }
    }
}

pub(super) fn definitions<'a>(input: Option<&'a CompletedSpillEvidence<'a>>) -> Result<DefinitionIndex<'a>> {
    let mut index = BTreeMap::new();
    for row in input.into_iter().flat_map(|input| input.rows) {
        let TelemetryRecord::ObjectDefinition(record) = row.record else { return Err(invalid()) };
        let (context, statement) = match &record.fact {
            ObjectDefinitionFact::MssqlCachedDefinition(definition) => (&definition.context, &definition.statement),
            ObjectDefinitionFact::MssqlRetainedPlan(definition) => (&definition.context, &definition.statement),
            _ => return Err(invalid()),
        };
        let key = (context.database_id, context.identity.key().map_err(|_| invalid())?);
        if index.insert(key, (statement, &row.reference)).is_some() {
            return Err(invalid());
        }
    }
    Ok(index)
}

pub(super) fn bind(
    runtime: &NativeReadContext,
    definitions: Option<&NativeReadContext>,
    index: &DefinitionIndex<'_>,
    sample: &CachedStatementRuntime,
) -> Result<Binding> {
    let Some(definitions) = definitions else {
        return Ok(Binding::unavailable(CompletedSpillStatementBinding::NotRequested));
    };
    if runtime.target.telemetry_id().map_err(|_| invalid())?
        != definitions.target.telemetry_id().map_err(|_| invalid())?
        || (runtime.has_qualified_local_owner()
            && definitions.has_qualified_local_owner()
            && runtime.partition != definitions.partition)
        || matches!((runtime.database_writable, definitions.database_writable), (Some(left), Some(right)) if left != right)
    {
        return Ok(Binding::unavailable(CompletedSpillStatementBinding::SourceChanged));
    }
    if !runtime.has_qualified_local_owner()
        || !definitions.has_qualified_local_owner()
        || runtime.partition != definitions.partition
        || runtime.database_writable != definitions.database_writable
    {
        return Ok(Binding::unavailable(CompletedSpillStatementBinding::SourceUnverified));
    }
    if !sample.measured() || !sample.context.identity.has_sql_handle() {
        return Ok(Binding::unavailable(CompletedSpillStatementBinding::SourceUnverified));
    }
    let key = (sample.context.database_id, sample.context.identity.key().map_err(|_| invalid())?);
    let Some((definition, reference)) = index.get(&key) else {
        return Ok(Binding::unavailable(CompletedSpillStatementBinding::DefinitionNotObserved));
    };
    let (preview, truncated, unavailable) = match &definition.payload {
        DefinitionPayload::Available { text, .. } => (
            Some(text.chars().take(STATEMENT_PREVIEW_CHARACTERS).collect()),
            text.chars().count() > STATEMENT_PREVIEW_CHARACTERS,
            None,
        ),
        DefinitionPayload::Unavailable { reason } => (None, false, Some(*reason)),
    };
    Ok(Binding {
        state: CompletedSpillStatementBinding::Matched,
        reference: Some((*reference).clone()),
        preview,
        truncated,
        unavailable,
    })
}

pub(super) fn explanation(state: CompletedSpillStatementBinding) -> String {
    match state {
        CompletedSpillStatementBinding::NotRequested => t!("view.completed_spills.binding.not_requested"),
        CompletedSpillStatementBinding::Matched => t!("view.completed_spills.binding.matched"),
        CompletedSpillStatementBinding::DefinitionNotObserved => t!("view.completed_spills.binding.not_observed"),
        CompletedSpillStatementBinding::SourceUnverified => t!("view.completed_spills.binding.unverified"),
        CompletedSpillStatementBinding::SourceChanged => t!("view.completed_spills.binding.changed"),
    }
}
