//! A writer list is an observed original roster with selected native access proof, never a latch-owner list.
use culprit_core::{
    MaintenanceClassification, MaintenanceClassificationIssue, PageWaitWriterExplanation, PageWaitWriterReport, Result,
    SourceOutcome, SourceState, classify_maintenance, format, t, telemetry::*,
};
use std::collections::BTreeSet;
mod insert_hotspot;
use insert_hotspot::EndOfLeafInsert;

struct ObservedWriter<'a> {
    original: &'a PageWaitWriterRequest,
    current: &'a PageWaitWriterCandidate,
    request: Option<&'a ActivityObservation>,
    request_state: PageWaitRequestState,
    relation: PageWriterRelation,
    maintenance: MaintenanceClassification,
}

impl<'a> ObservedWriter<'a> {
    fn admit(
        original: &'a PageWaitWriterRequest,
        current: &'a PageWaitWriterCandidate,
        engine: culprit_core::Engine,
        disqualified: &BTreeSet<NativeActivityIdentity>,
    ) -> Self {
        let request = current.request.as_ref().filter(|request| {
            request.has_session_generation_authority() && !disqualified.contains(&request.key.session)
        });
        let request_state = if current.request.is_some() && request.is_none() {
            PageWaitRequestState::Unverified
        } else {
            current.request_state
        };
        let relation = if request_state == PageWaitRequestState::SameStatement {
            current.relation
        } else {
            PageWriterRelation::Unavailable
        };
        let maintenance = request.map_or(
            MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::NoCurrentRequest },
            |request| classify_maintenance(engine, request),
        );
        Self { original, current, request, request_state, relation, maintenance }
    }

    fn has_insert_target(&self) -> bool {
        self.relation == PageWriterRelation::TargetObjectWrite
            && self.request.is_some_and(|request| request.command.as_deref() == Some("INSERT"))
            && !matches!(self.maintenance, MaintenanceClassification::Maintenance { .. })
            && self.current.binding.as_ref().is_some_and(|proof| {
                !proof.accesses.is_empty()
                    && proof.accesses.iter().all(|access| access.operation == PageWriteOperation::Insert)
            })
    }
}

pub fn explain_writers(
    focus: &PageWaitWriterFocus,
    context: &NativeReadContext,
    observation: Option<&PageWaitWriterObservation>,
    source: Option<&SourceOutcome>,
) -> Result<PageWaitWriterReport> {
    if !focus.is_valid()
        || !context.has_qualified_local_owner()
        || context.target != focus.waiter.participant().target
        || context.partition != focus.waiter.participant().partition
        || context.database_writable != focus.waiter.participant().database_writable
        || source.is_some_and(|source| !matches!(&focus.page.native_key,NativeRecordIdentity::Operational {source:expected,..} if expected==&source.source))
        || observation.is_some_and(|row| !focus.accepts_observation(context, row))
        || observation.is_some()
            && source.is_some_and(|source| {
                matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable)
            })
    {
        return Err(culprit_core::Error::invalid(t!("view.page_writers.invalid")));
    }
    let mut limitations = vec![
        t!("view.page_writers.limit.roster"),
        t!("view.page_writers.limit.access"),
        t!("view.page_writers.limit.ownership"),
        t!("view.page_writers.limit.execution"),
        t!("view.page_writers.limit.current"),
    ];
    if observation.is_none() {
        limitations.push(t!("view.page_writers.limit.read"));
    }
    if source.is_none() {
        limitations.push(t!("view.page_writers.limit.source"));
    } else if source.is_some_and(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.page_writers.limit.partial"));
    }
    let disqualified = ActivityEvidence::disqualified_session_generations(
        observation
            .into_iter()
            .flat_map(|row| row.candidates.iter().filter_map(|candidate| candidate.request.as_ref())),
    )?;
    let hotspot = observation.and_then(|row| EndOfLeafInsert::from_observation(row, source));
    let entries = observation
        .into_iter()
        .flat_map(|row| focus.candidates.iter().zip(&row.candidates))
        .map(|(original, current)| {
            let case = ObservedWriter::admit(original, current, context.target.engine(), &disqualified);
            render(case, context.target.engine(), hotspot)
        })
        .collect::<Vec<_>>();
    let writers = entries.iter().filter(|entry| entry.relation == PageWriterRelation::TargetObjectWrite).count();
    Ok(PageWaitWriterReport {
        source_state: source.map(|source| source.state).map(|state| {
            if observation.is_none() && matches!(state, SourceState::Collected | SourceState::Empty) {
                SourceState::Partial
            } else {
                state
            }
        }),
        source_reason: source.and_then(|source| source.reason.clone()),
        original_page: focus.page.clone(),
        original_target: focus.target.clone(),
        focus_key: focus.key()?,
        target_state: observation.map(|row| row.target_state),
        title: t!("view.page_writers.title"),
        summary: observation.map_or_else(
            || t!("view.page_writers.summary.unavailable"),
            |row| {
                format!(
                    "{} {}",
                    target_state(row.target_state),
                    t!(
                        "view.page_writers.summary.observed",
                        count = format::digits(writers.to_string()),
                        candidates = format::digits(entries.len().to_string())
                    )
                )
            },
        ),
        limitations,
        entries,
    })
}

fn render(
    case: ObservedWriter<'_>,
    engine: culprit_core::Engine,
    hotspot: Option<EndOfLeafInsert>,
) -> PageWaitWriterExplanation {
    let hotspot = hotspot.filter(|_| case.has_insert_target());
    let ObservedWriter { original, current, request, request_state, relation, maintenance } = case;
    let (explanation, mut investigation) = match request_state {
        PageWaitRequestState::Missing => {
            (t!("view.page_writers.request.missing"), t!("view.page_writers.investigate.missing"))
        }
        PageWaitRequestState::Changed => {
            (t!("view.page_writers.request.changed"), t!("view.page_writers.investigate.missing"))
        }
        PageWaitRequestState::Unverified => {
            (t!("view.page_writers.request.unverified"), t!("view.page_writers.investigate.unavailable"))
        }
        PageWaitRequestState::SameStatement => match relation {
            PageWriterRelation::TargetObjectWrite => {
                (t!("view.page_writers.relation.target"), t!("view.page_writers.investigate.target"))
            }
            PageWriterRelation::ReadOnlyPlan => {
                (t!("view.page_writers.relation.read_only"), t!("view.page_writers.investigate.other"))
            }
            PageWriterRelation::OtherTarget => {
                (t!("view.page_writers.relation.other"), t!("view.page_writers.investigate.other"))
            }
            PageWriterRelation::Unsupported => {
                (t!("view.page_writers.relation.unsupported"), t!("view.page_writers.investigate.unavailable"))
            }
            PageWriterRelation::Unavailable => {
                (t!("view.page_writers.relation.unavailable"), t!("view.page_writers.investigate.unavailable"))
            }
        },
    };
    let mut details = vec![explanation];
    if matches!(maintenance, MaintenanceClassification::Maintenance { .. }) {
        details.push(t!("view.page_writers.maintenance"));
    }
    let mut verify = t!("view.page_writers.verify");
    if let Some(hotspot) = hotspot {
        details.push(t!("view.page_writers.insert_hotspot.candidate"));
        if hotspot.leading_key_is_identity() {
            details.push(t!("view.page_writers.insert_hotspot.identity"));
        }
        match hotspot.sequential_key_setting() {
            Some(true) => details.push(t!("view.page_writers.insert_hotspot.pacing_on")),
            Some(false) => details.push(t!("view.page_writers.insert_hotspot.pacing_off")),
            None => {}
        }
        investigation = t!("view.page_writers.insert_hotspot.investigate");
        verify = t!("view.page_writers.insert_hotspot.verify");
    }
    PageWaitWriterExplanation {
        parent: original.parent.clone(),
        request: original.key.clone(),
        statement: original.statement_identity.clone(),
        request_state,
        relation,
        sql: request.map(|request| match &request.statement {
            DefinitionPayload::Available { text, .. } => DefinitionPayload::sql(text, engine),
            unavailable => unavailable.clone(),
        }),
        client: request.and_then(|request| request.client.clone()),
        plan_identity: request.and(current.plan_identity.as_ref()).cloned(),
        accesses: request.and(current.binding.as_ref()).map_or_else(Vec::new, |binding| binding.accesses.clone()),
        maintenance,
        title: t!(
            "view.page_writers.entry.title",
            session = original.key.session.id.as_str(),
            request = original.key.request.as_ref().map(|request| request.id.as_str()).unwrap_or_default()
        ),
        explanation: details.join(" "),
        investigation,
        verify,
    }
}

fn target_state(state: PageWriterTargetState) -> String {
    match state {
        PageWriterTargetState::CurrentMapped => t!("view.page_writers.target.current"),
        PageWriterTargetState::NoLongerObserved => t!("view.page_writers.target.gone"),
        PageWriterTargetState::ChangedTarget => t!("view.page_writers.target.changed"),
        PageWriterTargetState::Unavailable => t!("view.page_writers.target.unavailable"),
    }
}
