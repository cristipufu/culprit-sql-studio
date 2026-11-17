//! Identity text uses the typed native relationship; a display PID is never a deduplication or ownership key.
use super::*;
use culprit_core::{
    BlockingReport,
    telemetry::{ActivityKey, ActivityWaitOwner, TransactionWaitOwner},
};

const RELATIONSHIP_DETAIL_CHARS: usize = 2048;

pub(crate) fn blocking(report: &BlockingReport) -> String {
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &[],
        },
        |text| {
            line(
                text,
                &t!(
                    "evidence.cli.blocking_bounds",
                    source = &report.source,
                    from = &report.context.observed_from,
                    until = &report.context.observed_until,
                    original = &report.original.record_id
                ),
            );
            line(
                text,
                &t!(
                    "evidence.cli.blocking_counts",
                    native = format::digits(report.native_rows.to_string()),
                    observed = format::digits(report.observed_rows.to_string()),
                    retained = format::digits(report.relationships.len().to_string()),
                    shown =
                        format::digits(report.relationships.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
                ),
            );
            let relationships = report
                .relationships
                .iter()
                .take(super::super::BRIEF_FINDING_LIMIT)
                .map(|edge| {
                    t!(
                        "evidence.cli.blocking_relationship",
                        waiter = request(&edge.waiter),
                        owner = owner(&edge.owner),
                        context = if edge.owner_observed {
                            t!("evidence.cli.blocking_context_retained")
                        } else {
                            t!("evidence.cli.blocking_context_unavailable")
                        }
                    )
                })
                .collect::<Vec<_>>();
            super::super::append_brief_details(
                text,
                relationships.iter().map(String::as_str),
                RELATIONSHIP_DETAIL_CHARS,
            );
            if let Some(graph) = &report.blocking {
                line(
                    text,
                    &t!(
                        "evidence.cli.blocking_chains",
                        retained = format::digits(graph.chains.len().to_string()),
                        shown = format::digits(graph.chains.len().min(super::super::BRIEF_FINDING_LIMIT).to_string())
                    ),
                );
                let chains = graph
                    .chains
                    .iter()
                    .take(super::super::BRIEF_FINDING_LIMIT)
                    .map(|chain| {
                        t!("evidence.cli.observation_entry", title = &chain.headline, explanation = &chain.advice)
                    })
                    .collect::<Vec<_>>();
                super::super::append_brief_details(text, chains.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS);
            }
            for limitation in &report.limitations {
                line(text, limitation);
            }
            line(text, &report.investigation);
            line(text, &report.verify);
        },
    )
}

pub(super) fn request(key: &ActivityKey) -> String {
    let session = session(&key.session.id, Some(&key.session.generation));
    match &key.request {
        Some(request) => t!(
            "evidence.cli.blocking_request",
            session = session,
            id = format!("{:?}", request.id),
            generation = format!("{:?}", request.generation)
        ),
        None => session,
    }
}
fn session(id: &str, generation: Option<&str>) -> String {
    match generation {
        Some(generation) => {
            t!("evidence.cli.blocking_session", id = format!("{id:?}"), generation = format!("{generation:?}"))
        }
        None => t!("evidence.cli.blocking_session_unknown", id = format!("{id:?}")),
    }
}
fn owner(owner: &ActivityWaitOwner) -> String {
    match owner {
        ActivityWaitOwner::Session { id, generation } => session(id, generation.as_deref()),
        ActivityWaitOwner::Transaction { native_id, owner } => {
            let id = format!("{native_id:?}");
            match owner {
                TransactionWaitOwner::Active => t!("evidence.cli.blocking_owner.active", id = id),
                TransactionWaitOwner::Prepared => t!("evidence.cli.blocking_owner.prepared", id = id),
                TransactionWaitOwner::Orphaned => t!("evidence.cli.blocking_owner.orphaned", id = id),
                TransactionWaitOwner::Recovery => t!("evidence.cli.blocking_owner.recovery", id = id),
            }
        }
        ActivityWaitOwner::Unknown { native_id, .. } => {
            t!("evidence.cli.blocking_owner.unknown", id = format!("{native_id:?}"))
        }
    }
}
pub(super) fn line(text: &mut String, value: &str) {
    text.push('\n');
    text.extend(value.chars().take(OBSERVATION_DETAIL_CHARS));
    if value.chars().count() > OBSERVATION_DETAIL_CHARS {
        text.push('…');
    }
}

#[cfg(test)]
mod tests;
