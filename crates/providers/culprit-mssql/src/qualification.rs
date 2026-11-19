//! Source-owned assessment of actual captured work; no client timer, cap or unrelated fixture grants approval.
use culprit_core::{RecipeQualificationAssessment, RecipeQualificationGap, Result, telemetry::*};

/// The normal admitted native reader supplied these counters and settled its own session. The private proof is
/// inspected here at its actual source scope; only its existing numerical allowlist reaches operator output.
/// A complete capture window does not establish peak grants or confirmed active cancellation.
/// Therefore no supported full-profile acceptance policy can currently issue a Measured receipt.
pub fn assess_recipe_qualification(
    manifest: &CollectionManifest,
    source: &CollectionDescription,
) -> Result<RecipeQualificationAssessment> {
    let mut gaps = Vec::new();
    if !source.observation.snapshot().is_some_and(|snapshot| snapshot.dataset == SnapshotDataset::QueryCompilation) {
        gaps.push(RecipeQualificationGap::UnsupportedNativeProfile);
    }
    match measured_interval(manifest, source) {
        Some(interval) => {
            if interval.first_query_ordinal != 0 || interval.last_query_ordinal != Some(manifest.source_queries) {
                gaps.push(RecipeQualificationGap::PartialNativeMeasurementWindow);
            }
        }
        _ => gaps.push(RecipeQualificationGap::NativeMeasurementsUnavailable),
    }
    // No native peak-grant or exact-template active-cancellation experiment accompanies this normal capture.
    gaps.push(RecipeQualificationGap::NativeGrantEvidenceUnavailable);
    gaps.push(RecipeQualificationGap::ActiveCancellationUnproved);
    Ok(RecipeQualificationAssessment::Refuse(gaps))
}

fn measured_interval(
    manifest: &CollectionManifest,
    source: &CollectionDescription,
) -> Option<Box<MssqlSessionIntervalSummary>> {
    if manifest.description.as_ref() != Some(source) {
        return None;
    }
    let metrics = manifest.read_metrics.as_ref()?;
    if metrics.has_unconfirmed_work() {
        return None;
    }
    let native = metrics.native_session_work.as_deref()?;
    let NativeSessionReadWork::MssqlSession(work) = native;
    let database = work.interval.as_ref()?.context.target.database()?;
    let original = source.target.database()?;
    // Query Store owns a logical database; the independently qualified reader owns its physical session.
    // Match the original native database namespace without inventing a physical dataset owner.
    if database.incarnation.as_ref().is_none_or(|value| value.trim().is_empty())
        || original.incarnation.as_ref().is_none_or(|value| value.trim().is_empty())
        || database.telemetry_id().ok()? != original.telemetry_id().ok()?
    {
        return None;
    }
    let NativeSessionReadSummary::MssqlSession { state, interval, .. } =
        metrics.summary(Some(source)).ok()?.native_session_work.map(|work| *work)?;
    if state != NativeReadWorkState::Settled {
        return None;
    }
    let interval = interval?;
    let measured = ObservationBounds::new(&interval.observed_from, interval.observed_until.as_deref()?)?;
    let captured = source.observation.snapshot()?;
    ObservationBounds::new(&captured.observed_from, &captured.observed_until)?;
    (measured.contains(&captured.observed_from) && measured.contains(&captured.observed_until)).then_some(interval)
}
