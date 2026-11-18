//! Source-owned assessment of actual captured work; no client timer, cap or unrelated fixture grants approval.
use culprit_core::{RecipeQualificationAssessment, RecipeQualificationGap, Result, telemetry::*};

/// The normal admitted native reader supplied these counters and settled its own session. The private proof is
/// inspected here at its actual source scope; only its existing numerical allowlist reaches operator output.
/// Current session points bracket part of the acquisition, and Tiberius active cancellation remains unproved.
/// Therefore no supported full-profile acceptance policy can currently issue a Measured receipt.
pub fn assess_recipe_qualification(
    manifest: &CollectionManifest,
    source: &CollectionDescription,
) -> Result<RecipeQualificationAssessment> {
    let mut gaps = Vec::new();
    if !source.observation.snapshot().is_some_and(|snapshot| snapshot.dataset == SnapshotDataset::QueryCompilation) {
        gaps.push(RecipeQualificationGap::UnsupportedNativeProfile);
    }
    match manifest.read_metrics.as_ref().and_then(|metrics| metrics.native_session_work.as_deref()) {
        Some(NativeSessionReadWork::MssqlSession(work))
            if work.settlement == NativeReadSettlement::Quiescent && work.issue.is_none() =>
        {
            if work.interval.as_ref().is_none_or(|interval| {
                interval.initial.query_ordinal > 1
                    || interval.latest.as_ref().is_none_or(|point| point.query_ordinal < manifest.source_queries)
            }) {
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
