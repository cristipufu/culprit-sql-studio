//! Native optimizer admission delays affected requests. Shared gateway activity does not identify their pool or owners.
use super::source_coverage;
use culprit_core::{
    CompilerAdmissionReport, CompilerGatewayExplanation, CompilerWaiterExplanation, Result, SourceOutcome, format, t,
    telemetry::{CompilerAdmissionSnapshot, CompilerGatewayFacts, OperationalFact},
};

pub fn explain(
    snapshot: &CompilerAdmissionSnapshot,
    source: Option<&SourceOutcome>,
) -> Result<CompilerAdmissionReport> {
    if !snapshot.is_valid()
        || !source_coverage::consistent_source(source, snapshot.observations.iter().map(|row| row.source.as_str()))
    {
        return Err(culprit_core::Error::invalid(t!("view.compiler_admission.invalid")));
    }
    let mut waiters = Vec::new();
    let mut gateways = Vec::new();
    for row in &snapshot.observations {
        match &row.fact {
            OperationalFact::CompilerWaiter(waiter) => waiters.push(CompilerWaiterExplanation {
                observation_key: row.key.clone(),
                activity: waiter.request.key.clone(),
                title: t!(
                    "view.compiler_admission.waiter.title",
                    request = &waiter
                        .request
                        .key
                        .request
                        .as_ref()
                        .ok_or_else(|| culprit_core::Error::invalid(t!("view.compiler_admission.invalid")))?
                        .id
                ),
                explanation: t!("view.compiler_admission.waiter.explanation"),
                investigation: t!("view.compiler_admission.waiter.investigate"),
                verify: t!("view.compiler_admission.waiter.verify"),
            }),
            OperationalFact::CounterSet(_) => {
                let facts = CompilerGatewayFacts::from_observation(row)?;
                let mut explanation = vec![t!(
                    "view.compiler_admission.gateway.counts",
                    active = format::digits(facts.active_compilers.to_string()),
                    maximum = format::digits(facts.maximum_compilers.to_string()),
                    waiting = format::digits(facts.waiting_compilers.to_string())
                )];
                explanation.push(if facts.required {
                    t!("view.compiler_admission.gateway.required")
                } else {
                    t!("view.compiler_admission.gateway.bypassed")
                });
                gateways.push(CompilerGatewayExplanation {
                    observation_key: row.key.clone(),
                    identity: facts.identity.clone(),
                    title: t!(
                        "view.compiler_admission.gateway.title",
                        gate = &facts.identity.id,
                        pool = facts.resource_pool_id
                    ),
                    explanation: explanation.join(" "),
                });
            }
            _ => return Err(culprit_core::Error::invalid(t!("view.compiler_admission.invalid"))),
        }
    }
    let mut limitations = vec![
        t!("view.compiler_admission.limit.ownership"),
        t!("view.compiler_admission.limit.memory"),
        t!("view.compiler_admission.limit.clock"),
        t!("view.compiler_admission.limit.history"),
    ];
    if snapshot.context.as_ref().is_none_or(|context| !context.has_qualified_local_owner()) {
        limitations.push(t!("view.compiler_admission.limit.context"));
    }
    limitations.extend(super::memory_participants::source_limitation(source));
    Ok(CompilerAdmissionReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.compiler_admission.title"),
        summary: if snapshot.observations.is_empty() {
            t!("view.compiler_admission.summary.empty")
        } else if waiters.is_empty() {
            t!("view.compiler_admission.summary.gateways")
        } else {
            t!("view.compiler_admission.summary.waiter")
        },
        limitations,
        waiters,
        gateways,
    })
}

#[cfg(test)]
mod tests;
