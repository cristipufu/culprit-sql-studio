//! Original prerequisite bodies are authenticated; query-history body visibility is a separate bounded read.
use super::*;

pub(super) async fn authenticate(store: &dyn RecoveryStore, intent: &DiagnosticInvestigationIntent) -> Result<()> {
    let DiagnosticFocus::SourceRead { focus } = &intent.focus else { return Err(invalid()) };
    let original = load_diagnostic_evidence(store, &focus.basis).await?;
    if !intent.matches_parent_record(original.source(), original.record()) {
        return Err(invalid());
    }
    let Some(prerequisite) = focus.prerequisite.compilation_admission() else { return Ok(()) };
    let advertised = store.capture_manifest(&prerequisite.observation.capture).await?;
    verify_reference(&advertised, &prerequisite.observation)?;
    // Guard the native family before opening its body. This helper cannot recurse through another SourceRead.
    if !matches!(advertised.source.observation, SourceObservation::FocusedCompilationAdmission { .. }) {
        return Err(invalid());
    }
    let range = recovery::reopen(store, &advertised.range_id).await?;
    if range.manifest_digest() != advertised.digest()?
        || capture_reference(range.manifest())? != prerequisite.observation.capture_ref()
    {
        return Err(invalid());
    }
    let rows = input_body(store, range.manifest()).await?;
    let input = RecoveredDiagnosticInput { range, rows };
    let SourceObservation::FocusedCompilationAdmission { result, .. } = &input.range.manifest().source.observation
    else {
        return Err(invalid());
    };
    let gateway_parent = load_diagnostic_evidence(store, &result.focus.parent).await?;
    let TelemetryRecord::Activity(actor) = &gateway_parent.record().record else { return Err(invalid()) };
    if !result.focus.matches_parent(gateway_parent.source(), actor, &result.focus.parent) {
        return Err(invalid());
    }
    let referenced = input_rows(&input)?;
    let points = referenced
        .iter()
        .map(|(reference, row)| match &row.record {
            TelemetryRecord::Operational(native) => Ok((reference, native)),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>>>()?;
    if !focus.matches_prerequisite_input(original.source(), original.record(), &input.range.manifest().source, points) {
        return Err(invalid());
    }
    Ok(())
}
