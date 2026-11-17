//! One interpretation of the existing capture recorder; no database access or server-cost inference.
use culprit_core::{
    Result, SourceOverheadEvidence, SourceOverheadReport, format, t,
    telemetry::{CollectionStatus, DefinitionPayload, DeliveryProblem, ObservationBounds, PayloadUnavailable},
};

pub fn explain(evidence: &SourceOverheadEvidence) -> Result<SourceOverheadReport> {
    validate(evidence)?;
    let mut evidence = evidence.clone();
    let mut limitations = vec![
        t!("view.source_overhead.limit.client"),
        t!("view.source_overhead.limit.native"),
        t!("view.source_overhead.limit.window"),
        t!("view.source_overhead.limit.preview"),
    ];
    if evidence.source_queries.is_none() {
        limitations.push(t!("view.source_overhead.limit.total"));
    }
    if evidence.status != CollectionStatus::Complete {
        limitations.push(t!("view.source_overhead.limit.incomplete"));
    }
    let (template_order, summary) = match &mut evidence.read_metrics {
        None => (vec![], t!("view.source_overhead.summary.not_recorded")),
        Some(metrics) => {
            if metrics.active_tasks > 0 {
                limitations.push(t!(
                    "view.source_overhead.limit.active",
                    count = format::digits(metrics.active_tasks.to_string())
                ));
            }
            if metrics.untracked_tasks > 0 {
                limitations.push(t!(
                    "view.source_overhead.limit.untracked",
                    count = format::digits(metrics.untracked_tasks.to_string())
                ));
            }
            let unconfirmed =
                metrics.templates.iter().map(|metric| u64::from(metric.unconfirmed_cancellations)).sum::<u64>();
            if unconfirmed > 0 {
                limitations.push(t!(
                    "view.source_overhead.limit.unconfirmed",
                    count = format::digits(unconfirmed.to_string())
                ));
            }
            for metric in &mut metrics.templates {
                // A valid payload SHA authenticates bytes, not privacy or provider-owned SQL-template admission.
                if metric.statement.is_available() {
                    metric.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                }
            }
            let mut order = (0..metrics.templates.len()).map(|index| index as u32).collect::<Vec<_>>();
            order.sort_by(|left, right| {
                let left = &metrics.templates[*left as usize];
                let right = &metrics.templates[*right as usize];
                right
                    .client_elapsed_ms
                    .total_cmp(&left.client_elapsed_ms)
                    .then_with(|| left.template_id.cmp(&right.template_id))
            });
            let completed = metrics.templates.iter().map(|metric| u64::from(metric.completed_tasks)).sum::<u64>();
            let summary = t!(
                "view.source_overhead.summary.recorded",
                templates = format::digits(metrics.templates.len().to_string()),
                completed = format::digits(completed.to_string())
            );
            (order, summary)
        }
    };
    Ok(SourceOverheadReport { evidence, template_order, title: t!("view.source_overhead.title"), summary, limitations })
}

fn validate(evidence: &SourceOverheadEvidence) -> Result<()> {
    let until = evidence.finished_at.as_deref().unwrap_or(&evidence.started_at);
    if evidence.started_at.len() > 64
        || until.len() > 64
        || ObservationBounds::new(&evidence.started_at, until).is_none()
        || (evidence.status == CollectionStatus::Complete && evidence.finished_at.is_none())
        || evidence.read_metrics.as_ref().is_some_and(|metrics| !metrics.is_valid())
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
