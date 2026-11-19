//! A recorded native queue point is independent of the scan's unclocked aggregate signals.
use super::*;
use crate::support::request_memory::{Participant, qualified_participants};
use culprit_core::{Actionability, MemoryParticipantMechanism, SourceState, WorkloadDependency, telemetry::*};

const MAX_AFFECTED_REQUESTS: usize = 3;
const MAX_DISPLAYED_CONTEXTS: usize = 5;
const MAX_STATEMENT_CHARACTERS: usize = 512;
const MIN_CONCURRENT_RESERVATIONS: usize = 2;

pub(super) struct RecordedQueue<'s> {
    captured: &'s RequestMemorySnapshot,
    source_state: SourceState,
    participants: Vec<Participant<'s>>,
    waiting: Vec<usize>,
    reservations: Vec<Reservation>,
    investigation: QueueInvestigation,
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
    if sources.next().is_some() || !matches!(source.state, SourceState::Collected | SourceState::Partial) {
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
    let mut reservations = shared_reservations(&participants, &waiting, &database.database);
    reservations.sort_by_key(|reservation| {
        (reservation.role != ContextRole::OutputReservation, participants[reservation.index].key())
    });
    let investigation = investigation(&participants, &reservations);
    let identity = serde_json::to_vec(&(&captured.context, &captured.source, &captured.observed_at)).ok()?;
    Some(RecordedQueue {
        captured,
        source_state: source.state,
        participants,
        waiting,
        reservations,
        investigation,
        key: format!("recorded:{}", digest(&identity)),
    })
}

struct Reservation {
    index: usize,
    role: ContextRole,
}

#[derive(Clone, Copy)]
enum QueueInvestigation {
    Waiting,
    Concurrency,
    ClientOutput,
    ConcurrencyAndOutput,
}

fn shared_reservations(participants: &[Participant<'_>], waiting: &[usize], database: &str) -> Vec<Reservation> {
    participants
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            let observation = candidate.observation();
            observation.state == ExecutionGrantState::Granted
                && observation
                    .granted_kibibytes
                    .as_deref()
                    .is_some_and(|value| value.parse::<u64>().is_ok_and(|value| value > 0))
                && waiting.iter().any(|waiter| candidate.shares_admission_resource(&participants[*waiter], database))
        })
        .map(|(index, participant)| Reservation {
            index,
            role: if participant.mechanism() == Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait) {
                ContextRole::OutputReservation
            } else {
                ContextRole::Reservation
            },
        })
        .collect()
}

fn investigation(participants: &[Participant<'_>], reservations: &[Reservation]) -> QueueInvestigation {
    let mut resources = std::collections::BTreeMap::<_, std::collections::BTreeSet<_>>::new();
    for reservation in reservations {
        let participant = &participants[reservation.index];
        let resource = participant.resource().expect("a shared reservation has a qualified native resource");
        let request = participant.qualified_request().expect("a shared reservation has an original request identity");
        resources.entry(resource).or_default().insert(request);
    }
    let concurrent = resources.values().any(|requests| requests.len() >= MIN_CONCURRENT_RESERVATIONS);
    let output = reservations.iter().any(|reservation| reservation.role == ContextRole::OutputReservation);
    match (concurrent, output) {
        (false, false) => QueueInvestigation::Waiting,
        (true, false) => QueueInvestigation::Concurrency,
        (false, true) => QueueInvestigation::ClientOutput,
        (true, true) => QueueInvestigation::ConcurrencyAndOutput,
    }
}

pub(super) fn render(case: RecordedQueue<'_>) -> Finding {
    let database = case.captured.context.target.database().expect("the typed case requires a database target");
    let mut finding =
        finding("memory-queue", &case.key, Category::Configuration, subject(SubjectKind::Database, &database.database));
    finding.title = t!("check.memory_queue.recorded.title");
    finding.summary = if case.source_state == SourceState::Partial {
        t!(
            "check.memory_queue.recorded.summary.partial",
            at = &case.captured.observed_at,
            n = case.waiting.len(),
            count = format::count(case.waiting.len() as f64)
        )
    } else {
        t!(
            "check.memory_queue.recorded.summary",
            at = &case.captured.observed_at,
            count = format::count(case.waiting.len() as f64)
        )
    };
    if case.reservations.iter().any(|reservation| reservation.role == ContextRole::Reservation) {
        finding.summary.push(' ');
        finding.summary.push_str(&t!(
            "check.memory_queue.recorded.reservations",
            n = case.reservations.len(),
            count = format::count(case.reservations.len() as f64)
        ));
    }
    if matches!(case.investigation, QueueInvestigation::Concurrency | QueueInvestigation::ConcurrencyAndOutput) {
        finding.summary.push(' ');
        finding.summary.push_str(&t!("check.memory_queue.recorded.concurrency"));
    }
    if matches!(case.investigation, QueueInvestigation::ClientOutput | QueueInvestigation::ConcurrencyAndOutput) {
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
    let displayed = case
        .waiting
        .iter()
        .take(MAX_AFFECTED_REQUESTS)
        .map(|index| (*index, ContextRole::Waiting))
        .chain(case.reservations.iter().map(|reservation| (reservation.index, reservation.role)))
        .take(MAX_DISPLAYED_CONTEXTS)
        .collect::<Vec<_>>();
    for (index, role) in &displayed {
        append_context(&mut finding, &case.participants[*index], *role, case.captured.context.target.engine());
    }
    let omitted = case.waiting.len() + case.reservations.len() - displayed.len();
    if omitted > 0 {
        finding.caveats.push(t!("check.memory_queue.recorded.omitted", count = format::count(omitted as f64)));
    }
    finding.caveats.extend([
        t!("view.request_memory.limit.ownership"),
        t!("view.request_memory.limit.population"),
        t!("view.request_memory.limit.measurements"),
    ]);
    if case.source_state == SourceState::Partial {
        finding.caveats.push(t!("view.memory_participants.limit.source_partial"));
    }
    let (fix, verify) = match case.investigation {
        QueueInvestigation::Waiting => {
            (t!("view.request_memory.waiting.investigation"), vec![t!("view.request_memory.verify")])
        }
        QueueInvestigation::Concurrency => {
            (t!("check.memory_queue.recorded.concurrency_investigation"), vec![t!("view.request_memory.verify")])
        }
        QueueInvestigation::ClientOutput => {
            (t!("view.request_memory.output_wait.investigation"), vec![t!("view.request_memory.output_wait.verify")])
        }
        QueueInvestigation::ConcurrencyAndOutput => (
            format!(
                "{} {}",
                t!("check.memory_queue.recorded.concurrency_investigation"),
                t!("view.request_memory.output_wait.investigation")
            ),
            vec![t!("view.request_memory.verify"), t!("view.request_memory.output_wait.verify")],
        ),
    };
    finding.fix = Some(Fix { summary: fix, sql: None, cost: None });
    finding.verify = verify;
    finding
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextRole {
    Waiting,
    Reservation,
    OutputReservation,
}
fn append_context(finding: &mut Finding, participant: &Participant<'_>, role: ContextRole, engine: Engine) {
    let request = participant.request().expect("context attaches only to an admitted request");
    let native = request.key.request.as_ref().expect("an admitted grant request has its original request identity");
    let label = match role {
        ContextRole::Waiting => {
            t!("check.memory_queue.recorded.waiter", session = &request.key.session.id, request = &native.id)
        }
        ContextRole::Reservation => {
            t!("check.memory_queue.recorded.reservation", session = &request.key.session.id, request = &native.id)
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
        ContextRole::Reservation | ContextRole::OutputReservation => {
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
