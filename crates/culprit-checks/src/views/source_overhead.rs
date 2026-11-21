//! One interpretation of the existing capture recorder; no database access or server-cost inference.
use culprit_core::{
    Result, SourceOverheadEvidence, SourceOverheadReport, format, t,
    telemetry::{CollectionStatus, DeliveryProblem, ObservationBounds},
};

mod connections;
mod history;
mod native;
pub fn explain_snapshot(snapshot: &culprit_core::Snapshot) -> Vec<culprit_core::HistoricalSourceOverhead> {
    history::explain(snapshot)
}

pub fn explain(evidence: &SourceOverheadEvidence) -> Result<SourceOverheadReport> {
    validate(evidence)?;
    explain_summary(&evidence.summary()?)
}

/// Saved sanitized metadata shares the same interpretation; it can never reconstruct native authority.
pub fn explain_summary(evidence: &culprit_core::SourceOverheadCaptureSummary) -> Result<SourceOverheadReport> {
    if !evidence.is_valid() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let connection_work =
        evidence.read_metrics.as_ref().and_then(|metrics| metrics.connection_work.as_deref()).map(connections::explain);
    let evidence = evidence.clone();
    let native_interval =
        evidence.read_metrics.as_ref().and_then(|metrics| metrics.native_session_work.as_deref()).map(native::explain);
    let mut limitations = vec![
        t!("view.source_overhead.limit.client"),
        t!("view.source_overhead.limit.window"),
        t!("view.source_overhead.limit.preview"),
    ];
    if native_interval.is_none() {
        limitations.push(t!("view.source_overhead.limit.native"));
    }
    if evidence.source_queries.is_none() {
        limitations.push(t!("view.source_overhead.limit.total"));
    }
    if evidence.status != CollectionStatus::Complete {
        limitations.push(t!("view.source_overhead.limit.incomplete"));
    }
    let (template_order, summary) = match &evidence.read_metrics {
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
    Ok(SourceOverheadReport {
        connection_work,
        evidence,
        template_order,
        title: t!("view.source_overhead.title"),
        summary,
        limitations,
        native_interval,
    })
}

fn validate(evidence: &SourceOverheadEvidence) -> Result<()> {
    let until = evidence.finished_at.as_deref().unwrap_or(&evidence.started_at);
    if evidence.started_at.len() > 64
        || until.len() > 64
        || ObservationBounds::new(&evidence.started_at, until).is_none()
        || (evidence.status == CollectionStatus::Complete && evidence.finished_at.is_none())
        || evidence.read_metrics.as_ref().is_some_and(|metrics| {
            !metrics.is_valid()
                || metrics.connection_work.as_ref().is_some_and(|work| {
                    work.attempts.iter().flat_map(|attempt| &attempt.phases).any(|phase| {
                        let Some(bounds) = ObservationBounds::new(&evidence.started_at, until) else { return true };
                        !bounds.contains(&phase.client_started_at)
                            || phase.client_finished_at.as_ref().is_some_and(|at| !bounds.contains(at))
                    })
                })
        })
    {
        return Err(DeliveryProblem::Integrity.error());
    }
    Ok(())
}

/// Exact historical original. Current scan identity and clocks are deliberately not inputs.
pub fn explain_recorded(original: &culprit_core::CapturedSourceAcquisition) -> Result<SourceOverheadReport> {
    if !original.is_valid() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let mut report = explain_summary(&original.summary)?;
    report.limitations.push(t!("view.source_overhead.limit.local"));
    report.limitations.push(t!("view.source_overhead.limit.historical"));
    if original.seal == culprit_core::SourceAcquisitionSeal::Provisional {
        report.limitations.push(t!("view.source_overhead.limit.provisional"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
