//! Compilation context for an exact retained Query Store query is not a plan-regression or TVP verdict.
//! The current acquisition owner is independent of the failed historical execution population.
use super::*;
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum QueryCompilationGap {
    ActualExecutionPlan,
    RuntimeInputs,
    ComparableExecutionWork,
    BlockingAndGrantDemand,
    StatisticsAndForcingContext,
}
impl QueryCompilationGap {
    pub const ALL: &'static [Self] = &[
        Self::ActualExecutionPlan,
        Self::RuntimeInputs,
        Self::ComparableExecutionWork,
        Self::BlockingAndGrantDemand,
        Self::StatisticsAndForcingContext,
    ];
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum QueryCompilationSelection {
    Observed {
        observation: DiagnosticEvidenceRef,
        counters: Box<CompilationCounterPoint>,
    },
    /// The bounded retained roster supplied no row for this native query number. It proves no eviction/recovery.
    NotObserved,
    /// A row reused the native query number with another complete incarnation. It cannot replace the original.
    IncarnationChanged,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QueryCompilationAnswer {
    pub basis: DiagnosticEvidenceRef,
    pub basis_context: NativeReadContext,
    pub original_interval: TelemetryInterval,
    /// These remain the original Query Store references, never a current cache-handle or statement-text join.
    pub original_references: StatementReferences,
    pub query: NativeQueryIdentity,
    pub plan: NativePlanIdentity,
    pub cohort: NativeIdentity,
    pub execution_type: u8,
    pub result: DiagnosticCaptureRef,
    pub result_context: NativeReadContext,
    pub source_state: crate::SourceState,
    pub selection: QueryCompilationSelection,
    pub unresolved: Vec<QueryCompilationGap>,
}
impl QueryCompilationAnswer {
    /// Both captures and their canonical row rosters must be independently authenticated by the caller.
    /// The same canonical observer verifies the complete roster, retaining every selected native-query candidate
    /// and its original ordinal. A manually chosen prefix supplies no authentication or completeness.
    pub fn from_originals(
        focus: &SourceReadFocus,
        original_source: &CollectionDescription,
        original: &ExportedRecord,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    ) -> Result<Self> {
        let basis_context = focus.original_context(original_source)?;
        let SourceObservation::SourceRead { snapshot, focus_key, execution: result_context } = &source.observation
        else {
            return Err(invalid());
        };
        let execution = SourceReadExecutionScope::from_context(focus.question, result_context)?;
        let Some(DiagnosticOriginalCondition::MssqlQueryHistory { execution_type, query, plan, cohort }) =
            DiagnosticOriginalCondition::from_record(original_source, &original.record)
        else {
            return Err(invalid());
        };
        let TelemetryRecord::QueryRuntime(original_runtime) = &original.record else { return Err(invalid()) };
        let query = NativeQueryIdentity { query_id: query.id, qualifiers: query.qualifiers };
        let plan = NativePlanIdentity { plan_id: plan.id, query: query.clone(), qualifiers: plan.qualifiers };
        let history = original_source.query_history().ok_or_else(invalid)?;
        if focus.question != SourceReadQuestion::CompilationHistory
            || !matches!(focus.prerequisite, SourceReadPrerequisite::ObservedQueryHistoryFailure { .. })
            || !focus.matches_basis(original_source, original, &execution)
            || !source.partition_is_consistent()
            || !source.observation.covers_request()
            || source.target.engine() != crate::Engine::Mssql
            || focus.key()? != *focus_key
            || !capture.is_valid()
            || capture.capture == focus.basis.capture
            || source.captured_range()?.scope_id != capture.source_scope_id
            || snapshot.dataset != SnapshotDataset::QueryCompilation
            || (snapshot.records as usize) > MAX_QUERY_COMPILATION_OBSERVATIONS
            || (snapshot.records as usize) < rows.len()
            || rows.len() > MAX_QUERY_COMPILATION_OBSERVATIONS
            || query_store_database_namespace(&original_source.target, &original_source.partition)
                != query_store_database_namespace(&source.target, &source.partition)
            || ObservationBounds::new(&basis_context.observed_until, &result_context.observed_from).is_none()
        {
            return Err(invalid());
        }
        let selection = select_original_query(source, capture, rows, &query)?;
        let answer = Self {
            basis: focus.basis.as_ref().clone(),
            basis_context,
            original_interval: history.interval.clone(),
            original_references: original_runtime.key.references.clone(),
            query,
            plan,
            cohort,
            execution_type,
            result: capture.clone(),
            result_context: result_context.as_ref().clone(),
            source_state: snapshot.coverage_state()?,
            selection,
            unresolved: QueryCompilationGap::ALL.to_vec(),
        };
        if !answer.is_valid() {
            return Err(invalid());
        }
        Ok(answer)
    }

    pub fn is_valid(&self) -> bool {
        let scope = NativeSourceReadScope::from_context(&self.result_context);
        self.basis.is_valid()
            && self.basis.stream == TelemetryStream::QueryRuntime
            && matches!(self.basis.native_key, NativeRecordIdentity::Runtime(_))
            && self.result.is_valid()
            && self.result.capture != self.basis.capture
            && scope.is_ok_and(|scope| scope.matches_context(&self.basis_context))
            && self.result_context.target.engine() == crate::Engine::Mssql
            && ObservationBounds::new(&self.basis_context.observed_until, &self.result_context.observed_from).is_some()
            && valid_key(&self.original_interval.native_id)
            && ObservationBounds::new(&self.original_interval.from, &self.original_interval.until).is_some()
            && query_store_query_identity_is_complete(&self.query)
            && self.plan.is_valid()
            && self.plan.query == self.query
            && self.original_references.is_resolved()
            && self.original_references.query.as_ref().is_some_and(|reference| reference.native == self.query.native())
            && self.original_references.plan.as_ref().is_some_and(|reference| reference.native == self.plan.native())
            && self.cohort.is_valid()
            && matches!(self.execution_type, 3 | 4)
            && self.cohort.qualifiers.get("execution_type") == Some(&self.execution_type.to_string())
            && self.unresolved == QueryCompilationGap::ALL
            && match &self.selection {
                QueryCompilationSelection::Observed { observation, counters } => {
                    observation.is_valid()
                        && observation.capture_ref() == self.result
                        && counters.is_valid()
                        && MssqlQueryCompilation::identity_key(&counters.identity).ok()
                            == MssqlQueryCompilation::identity_key(&self.query).ok()
                        && matches!(&observation.native_key, NativeRecordIdentity::Operational { source, key }
                            if source == crate::source_id::QUERY_COMPILATION && key == &counters.observation_key)
                        && matches!(self.source_state, crate::SourceState::Collected | crate::SourceState::Partial)
                }
                QueryCompilationSelection::NotObserved => true,
                QueryCompilationSelection::IncarnationChanged => {
                    matches!(self.source_state, crate::SourceState::Collected | crate::SourceState::Partial)
                }
            }
    }

    pub fn references(&self) -> impl Iterator<Item = &DiagnosticEvidenceRef> {
        let selected = match &self.selection {
            QueryCompilationSelection::Observed { observation, .. } => Some(observation),
            _ => None,
        };
        std::iter::once(&self.basis).chain(selected)
    }
}

fn select_original_query(
    source: &CollectionDescription,
    capture: &DiagnosticCaptureRef,
    rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    original: &NativeQueryIdentity,
) -> Result<QueryCompilationSelection> {
    let expected = MssqlQueryCompilation::identity_key(original)?;
    let mut keys = BTreeSet::new();
    let mut selected = None;
    let mut query_number_seen = false;
    let mut previous = None;
    for (reference, row) in rows {
        if row.capture != capture.capture
            || reference.capture_ref() != *capture
            || previous.is_some_and(|ordinal| reference.row_ordinal <= ordinal)
        {
            return Err(invalid());
        }
        previous = Some(reference.row_ordinal);
        reference.validate_canonical_record(source, &row.record)?;
        let TelemetryRecord::Operational(native) = &row.record else { return Err(invalid()) };
        let OperationalFact::MssqlQueryCompilation(point) = &native.fact else { return Err(invalid()) };
        if native.source != crate::source_id::QUERY_COMPILATION || !keys.insert(point.identity.query_id.clone()) {
            return Err(invalid());
        }
        query_number_seen |= point.identity.query_id == original.query_id;
        if MssqlQueryCompilation::identity_key(&point.identity)? == expected {
            selected = Some(QueryCompilationSelection::Observed {
                observation: (*reference).clone(),
                counters: Box::new(CompilationCounterPoint::from_observation(native)?),
            });
        }
    }
    Ok(selected.unwrap_or(if query_number_seen {
        QueryCompilationSelection::IncarnationChanged
    } else {
        QueryCompilationSelection::NotObserved
    }))
}

impl DiagnosticEpisode {
    pub fn complete_query_compilation_read(
        &self,
        intent: &DiagnosticInvestigationIntent,
        original_source: &CollectionDescription,
        original: &ExportedRecord,
        source: &CollectionDescription,
        capture: &DiagnosticCaptureRef,
        rows: &[(&DiagnosticEvidenceRef, &ExportedRecord)],
    ) -> Result<Self> {
        if !intent.matches_episode(self) || !intent.matches_result_source(source) || !self.needs_question(&intent.focus)
        {
            return Err(invalid());
        }
        let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(invalid()) };
        let answer = QueryCompilationAnswer::from_originals(focus, original_source, original, source, capture, rows)?;
        self.transition_source_read(SourceReadStageState::Answered {
            answer: Box::new(SourceReadAnswer::QueryCompilation(Box::new(answer))),
        })
    }
}

#[cfg(test)]
mod tests;
