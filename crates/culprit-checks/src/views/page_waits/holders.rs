use culprit_core::{
    Error, Result, SourceOutcome, SourceState,
    page_wait_report::{PageTaskHolderBound, PageTaskHolderExplanation, PageTaskHolderReport},
    t,
    telemetry::*,
};

pub fn explain_task_holders(
    focus: &PageTaskHolderFocus,
    context: &NativeReadContext,
    observation: Option<&PageTaskHolderObservation>,
    source: Option<&SourceOutcome>,
) -> Result<PageTaskHolderReport> {
    if !focus.is_valid()
        || !context.has_qualified_local_owner()
        || context.target != focus.participant.target
        || context.partition != focus.participant.partition
        || context.database_writable != focus.participant.database_writable
        || source.is_some_and(|source| {
            !matches!(&focus.page.native_key,
            NativeRecordIdentity::Operational { source: expected, .. } if expected == &source.source)
        })
        || observation.is_some_and(|row| !focus.accepts_observation(context, row))
        || observation.is_some()
            && source.is_some_and(|source| {
                matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable)
            })
    {
        return Err(Error::invalid(t!("view.page_holders.invalid")));
    }
    let mut limitations = vec![
        t!("view.page_holders.limit.point"),
        t!("view.page_holders.limit.contribution"),
        t!("view.page_holders.limit.population"),
        t!("view.page_holders.limit.task"),
    ];
    if observation.is_none() {
        limitations.push(t!("view.page_holders.limit.read"));
    }
    if source.is_none() {
        limitations.push(t!("view.page_holders.limit.source"));
    }
    if source.is_some_and(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.page_holders.limit.partial"));
    }
    let entries = observation.map_or_else(Vec::new, |row| {
        vec![render(PageTaskHolderBound::Before, &row.before), render(PageTaskHolderBound::After, &row.after)]
    });
    Ok(PageTaskHolderReport {
        source_state: source.map(|source| {
            if observation.is_none() && matches!(source.state, SourceState::Collected | SourceState::Empty) {
                SourceState::Partial
            } else {
                source.state
            }
        }),
        source_reason: source.and_then(|source| source.reason.clone()),
        original_page: focus.page.clone(),
        focus_key: focus.key()?,
        target_state: observation.map(|row| row.target_state),
        final_target: observation.and_then(|row| row.final_target.clone()),
        title: t!("view.page_holders.title"),
        summary: observation.map_or_else(|| t!("view.page_holders.summary.unavailable"), summary),
        limitations,
        entries,
    })
}

fn summary(row: &PageTaskHolderObservation) -> String {
    if row.target_state != PageWriterTargetState::CurrentMapped {
        return match row.target_state {
            PageWriterTargetState::NoLongerObserved => t!("view.page_holders.summary.gone"),
            PageWriterTargetState::ChangedTarget => t!("view.page_holders.summary.changed"),
            PageWriterTargetState::Unavailable if row.page.is_some() || row.after_task.is_some() => {
                t!("view.page_holders.summary.mapping_unavailable")
            }
            _ => t!("view.page_holders.summary.unavailable"),
        };
    }
    match (&row.before.holder.request, &row.after.holder.request) {
        (Some(before), Some(after))
            if before.key == after.key && before.statement_identity == after.statement_identity =>
        {
            t!("view.page_holders.summary.same")
        }
        (Some(_), Some(_)) => t!("view.page_holders.summary.different"),
        (Some(_), None) | (None, Some(_)) => t!("view.page_holders.summary.one"),
        _ => t!("view.page_holders.summary.unresolved"),
    }
}

fn render(bound: PageTaskHolderBound, point: &MssqlPageTaskHolderPoint) -> PageTaskHolderExplanation {
    let (explanation, investigation) = if point.request != PageWaitRequestState::SameStatement {
        (t!("view.page_holders.request.changed"), t!("view.page_holders.investigate.changed"))
    } else if point.wait != PageWaitRelation::OriginalObserved {
        (t!("view.page_holders.wait.changed"), t!("view.page_holders.investigate.changed"))
    } else {
        relation(point.holder.relation)
    };
    let explanation = if point.holder.request.as_ref().is_some_and(|request| !request.statement.is_available()) {
        format!("{explanation} {}", t!("view.page_holders.sql.unavailable"))
    } else {
        explanation
    };
    PageTaskHolderExplanation {
        bound,
        observed_at: point.observed_at.clone(),
        request_state: point.request,
        wait_state: point.wait,
        relation: point.holder.relation,
        task: point.holder.task.clone(),
        request: point.holder.request.as_ref().map(|request| request.key.clone()),
        statement_identity: point.holder.request.as_ref().and_then(|request| request.statement_identity.clone()),
        statement: point.holder.request.as_ref().map(|request| request.statement.clone()),
        title: match bound {
            PageTaskHolderBound::Before => t!("view.page_holders.entry.before"),
            PageTaskHolderBound::After => t!("view.page_holders.entry.after"),
        },
        explanation,
        investigation,
        verify: t!("view.page_holders.verify"),
    }
}

fn relation(relation: PageTaskHolderRelation) -> (String, String) {
    let explanation = match relation {
        PageTaskHolderRelation::QualifiedRequest => t!("view.page_holders.relation.request"),
        PageTaskHolderRelation::TaskOnly => t!("view.page_holders.relation.task"),
        PageTaskHolderRelation::NotReported => t!("view.page_holders.relation.not_reported"),
        PageTaskHolderRelation::Sentinel => t!("view.page_holders.relation.sentinel"),
        PageTaskHolderRelation::OtherDatabase => t!("view.page_holders.relation.other_database"),
        PageTaskHolderRelation::Changed => t!("view.page_holders.relation.changed"),
        PageTaskHolderRelation::Ambiguous => t!("view.page_holders.relation.ambiguous"),
        PageTaskHolderRelation::SelfReference => t!("view.page_holders.relation.self"),
        PageTaskHolderRelation::Unavailable => t!("view.page_holders.relation.unavailable"),
    };
    let investigation = match relation {
        PageTaskHolderRelation::QualifiedRequest => t!("view.page_holders.investigate.request"),
        PageTaskHolderRelation::Changed | PageTaskHolderRelation::SelfReference => {
            t!("view.page_holders.investigate.changed")
        }
        _ => t!("view.page_holders.investigate.unresolved"),
    };
    (explanation, investigation)
}
