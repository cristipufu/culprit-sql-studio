//! A recorded native queue point is independent of the scan's unclocked aggregate signals.
use super::*;
use crate::support::request_memory::{Participant, qualified_participants};
use culprit_core::{Actionability, MemoryParticipantMechanism, SourceState, WorkloadDependency, telemetry::*};

const MAX_AFFECTED_REQUESTS: usize = 3;
const MAX_OUTPUT_RESERVATIONS: usize = 2;
const MAX_STATEMENT_CHARACTERS: usize = 512;

pub(super) struct RecordedQueue<'s> {
    captured: &'s RequestMemorySnapshot,
    participants: Vec<Participant<'s>>,
    waiting: Vec<usize>,
    output_reservations: Vec<usize>,
    key: String,
}

pub(super) fn decide(snapshot: &Snapshot) -> Option<RecordedQueue<'_>> {
    let captured = snapshot.pressure.as_ref()?.request_memory.as_ref()?;
    let database = captured.context.target.database()?;
    if !captured.is_valid()
        || !captured.context.has_qualified_local_owner()
        || !captured.context.target.snapshot_namespace_comparison(snapshot).allows_comparison()
    {
        return None;
    }
    let mut sources = snapshot.sources.iter().filter(|source| source.source == captured.source);
    let source = sources.next()?;
    if sources.next().is_some() || source.state != SourceState::Collected {
        return None;
    }
    let observations: Vec<_> = captured.observations.iter().collect();
    let mut participants = qualified_participants(&captured.context, &observations, Some(source)).ok()?;
    participants.sort_by(|left, right| left.key().cmp(right.key()));
    let waiting: Vec<_> = participants
        .iter()
        .enumerate()
        .filter(|(_, participant)| {
            participant.observation().state == ExecutionGrantState::Waiting && participant.request().is_some()
        })
        .map(|(index, _)| index)
        .collect();
    if waiting.is_empty() {
        return None;
    }
    let output_reservations = participants
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.mechanism() == Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)
                && waiting
                    .iter()
                    .any(|waiter| candidate.shares_admission_resource(&participants[*waiter], &database.database))
        })
        .map(|(index, _)| index)
        .collect();
    let identity = serde_json::to_vec(&(&captured.context, &captured.source, &captured.observed_at)).ok()?;
    Some(RecordedQueue {
        captured,
        participants,
        waiting,
        output_reservations,
        key: format!("recorded:{}", digest(&identity)),
    })
}

pub(super) fn render(case: RecordedQueue<'_>) -> Finding {
    let database = case.captured.context.target.database().expect("the typed case requires a database target");
    let mut finding =
        finding("memory-queue", &case.key, Category::Configuration, subject(SubjectKind::Database, &database.database));
    finding.title = t!("check.memory_queue.recorded.title");
    finding.summary = t!(
        "check.memory_queue.recorded.summary",
        at = &case.captured.observed_at,
        count = format::count(case.waiting.len() as f64)
    );
    if !case.output_reservations.is_empty() {
        finding.summary.push(' ');
        finding.summary.push_str(&t!("check.memory_queue.recorded.output_context"));
    }
    finding.actionability = Actionability::Information;
    finding.severity = Severity::Low;
    finding.confidence = Confidence::High;
    finding.impact.score = base_score(Severity::Low);
    finding
        .impact
        .set_keyed_label(key!("check.memory_queue.recorded.impact"), t!("check.memory_queue.recorded.impact"));
    finding.facts.workload_dependency = Some(WorkloadDependency::Independent);
    for index in case.waiting.iter().take(MAX_AFFECTED_REQUESTS) {
        append_context(
            &mut finding,
            &case.participants[*index],
            ContextRole::Waiting,
            case.captured.context.target.engine(),
        );
    }
    for index in case.output_reservations.iter().take(MAX_OUTPUT_RESERVATIONS) {
        append_context(
            &mut finding,
            &case.participants[*index],
            ContextRole::OutputReservation,
            case.captured.context.target.engine(),
        );
    }
    let omitted = case.waiting.len().saturating_sub(MAX_AFFECTED_REQUESTS)
        + case.output_reservations.len().saturating_sub(MAX_OUTPUT_RESERVATIONS);
    if omitted > 0 {
        finding.caveats.push(t!("check.memory_queue.recorded.omitted", count = format::count(omitted as f64)));
    }
    finding.caveats.extend([
        t!("view.request_memory.limit.ownership"),
        t!("view.request_memory.limit.population"),
        t!("view.request_memory.limit.measurements"),
    ]);
    let output_observed = !case.output_reservations.is_empty();
    finding.fix = Some(Fix {
        summary: if output_observed {
            t!("view.request_memory.output_wait.investigation")
        } else {
            t!("view.request_memory.waiting.investigation")
        },
        sql: None,
        cost: None,
    });
    finding.verify = vec![if output_observed {
        t!("view.request_memory.output_wait.verify")
    } else {
        t!("view.request_memory.verify")
    }];
    finding
}

#[derive(Clone, Copy)]
enum ContextRole {
    Waiting,
    OutputReservation,
}
fn append_context(finding: &mut Finding, participant: &Participant<'_>, role: ContextRole, engine: Engine) {
    let request = participant.request().expect("context attaches only to an admitted request");
    let native = request.key.request.as_ref().expect("an admitted grant request has its original request identity");
    let label = match role {
        ContextRole::Waiting => {
            t!("check.memory_queue.recorded.waiter", session = &request.key.session.id, request = &native.id)
        }
        ContextRole::OutputReservation => t!(
            "check.memory_queue.recorded.output_reservation",
            session = &request.key.session.id,
            request = &native.id
        ),
    };
    let statement = statement_preview(participant.current_statement(), engine);
    finding.metrics.push(Metric::text(label, statement).keyed(&format!("request-context:{}", participant.key())));
    let (memory, label) = match role {
        ContextRole::Waiting => {
            (&participant.observation().requested_kibibytes, t!("check.memory_queue.recorded.requested"))
        }
        ContextRole::OutputReservation => {
            (&participant.observation().granted_kibibytes, t!("check.memory_queue.recorded.granted"))
        }
    };
    if let Some(memory) = memory {
        finding.metrics.push(
            Metric::text(label, format::digits(memory.clone())).keyed(&format!("request-memory:{}", participant.key())),
        );
    }
    finding.metrics.push(
        Metric::text(
            t!("check.memory_queue.recorded.identity"),
            t!(
                "check.memory_queue.recorded.identity.value",
                session = &request.key.session.id,
                session_generation = &request.key.session.generation,
                request = &native.id,
                request_generation = &native.generation
            ),
        )
        .keyed(&format!("request-identity:{}", participant.key())),
    );
}

fn statement_preview(statement: Option<&DefinitionPayload>, engine: Engine) -> String {
    let safe = match statement {
        Some(DefinitionPayload::Available { text, .. }) => DefinitionPayload::sql(text, engine),
        _ => return t!("check.memory_queue.recorded.statement_unavailable"),
    };
    let DefinitionPayload::Available { text, .. } = safe else {
        return t!("check.memory_queue.recorded.statement_unavailable");
    };
    let mut preview: String = text.chars().take(MAX_STATEMENT_CHARACTERS).collect();
    if text.chars().count() > MAX_STATEMENT_CHARACTERS {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests;
