//! Admission decisions append revisions through the existing offline CAS owner, without replaying a baseline.
use super::*;

pub(crate) async fn set_analysis(
    outbox: &mut Outbox,
    parent: &DiagnosticEvidenceRef,
    episode_id: &str,
    status: DiagnosticAnalysisStatus,
    issue: Option<DiagnosticAnalysisIssue>,
) -> Result<()> {
    update_analysis(outbox, parent, episode_id, status, issue, None).await
}

pub(crate) async fn record_investigation(
    outbox: &mut Outbox,
    event: DiagnosticInvestigationEvent,
    status: DiagnosticAnalysisStatus,
    issue: Option<DiagnosticAnalysisIssue>,
) -> Result<()> {
    let parent = event.intent.focus.participant().parent.clone();
    let episode_id = event.intent.episode_id.clone();
    update_analysis(outbox, &parent, &episode_id, status, issue, Some(event)).await
}

async fn update_analysis(
    outbox: &mut Outbox,
    parent: &DiagnosticEvidenceRef,
    episode_id: &str,
    status: DiagnosticAnalysisStatus,
    issue: Option<DiagnosticAnalysisIssue>,
    event: Option<DiagnosticInvestigationEvent>,
) -> Result<()> {
    let input = outbox.diagnostic_input(&parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await?;
    if input.parent != parent.capture_ref() {
        return Err(DeliveryProblem::Integrity.error());
    }
    let row =
        input.rows.iter().find(|row| row.reference == *parent).ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let TelemetryRecord::Activity(original) = &row.row.record else {
        return Err(DeliveryProblem::Integrity.error());
    };
    let Some(mut checkpoint) = input.checkpoint.clone() else { return Ok(()) };
    let Some(episode) = checkpoint.episodes.iter_mut().find(|episode| episode.id == episode_id) else {
        return Ok(());
    };
    if !episode.analysis.needs_work() && event.is_none() {
        return Ok(());
    }
    let task_question = if let DiagnosticSignal::PageTaskWait { parent: expected, page, .. } = &episode.signal {
        if expected.as_ref() != parent {
            return Err(DeliveryProblem::Integrity.error());
        }
        let (source, native) = outbox.diagnostic_evidence(page).await?;
        let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(point), .. }) =
            &native.record
        else {
            return Err(DeliveryProblem::Integrity.error());
        };
        let plan = plan_page_task_holder_focus(&source, point, page, &input.activity_frame()?)?;
        match plan {
            DiagnosticFocusPlan::Ready(focus) => Some(focus),
            _ => return Err(DeliveryProblem::Integrity.error()),
        }
    } else {
        None
    };
    let matches_parent = match &event {
        Some(event) => {
            event.matches_episode(episode) && event.intent.focus.matches_parent(&input.source, original, parent)
        }
        None => match task_question {
            Some(focus) => episode.matches_focus(&focus),
            None => matches!(episode.plan_focus(&input.source, original, parent)?, DiagnosticFocusPlan::Ready(_)),
        },
    };
    if !matches_parent {
        return Err(DeliveryProblem::Integrity.error());
    }
    let previous = episode.clone();
    if episode.analysis.needs_work() {
        episode.analysis = status;
        episode.issue = issue;
    }
    let analyzed_at = chrono::Utc::now().to_rfc3339();
    let revision = match event {
        Some(event) => DiagnosticRevision::with_investigation(&previous, episode.clone(), event, &analyzed_at),
        None => DiagnosticRevision::between(Some(&previous), episode.clone(), &analyzed_at),
    }?;
    let Some(revision) = revision else {
        return Ok(());
    };
    *episode = revision.episode.clone();
    checkpoint.input = input.parent.clone();
    checkpoint.episodes.retain(DiagnosticEpisode::retained_in_checkpoint);
    let step_id = digest(
        &serde_json::to_vec(&("diagnostic-admission-analysis-v1", &revision.id, &input.parent))
            .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    let update = DiagnosticUpdate { step_id, analyzed_at, revisions: vec![revision], checkpoint };
    outbox.commit_diagnostics(input, update).await?;
    Ok(())
}
