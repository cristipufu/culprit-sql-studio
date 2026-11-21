//! One immutable source/coverage/position policy for the two native log-capture families.
use culprit_core::{Result, SourceState, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};
pub(super) struct QualifiedCapture<'a> {
    pub context: NativeReadContext,
    pub sources: Vec<StateSourceCoverage>,
    pub points: Vec<(&'a OperationalObservation, MssqlChangeCapturePoint<'a>)>,
}
pub(super) fn qualify<'a>(
    family: MssqlChangeCaptureFamily,
    source: &CollectionDescription,
    observations: &[&'a OperationalObservation],
    read_complete: bool,
) -> Result<QualifiedCapture<'a>> {
    let header = source.observation.snapshot().ok_or_else(invalid)?;
    let fallback = NativeReadContext::from_snapshot(source)?;
    if !family.valid_header(header)
        || source.target.engine() != culprit_core::Engine::Mssql
        || !fallback.has_qualified_local_owner()
        || source.continuity != SourceContinuity::Unverified
    {
        return Err(invalid());
    }
    let mut sources = header.sources.iter().map(|s| (s.source.as_str(), (s, 0usize))).collect::<BTreeMap<_, _>>();
    let mut points = Vec::new();
    let mut keys = BTreeSet::new();
    let mut original: Option<(u32, &NativeReadContext)> = None;
    for row in observations {
        let point = MssqlChangeCapturePoint::from_row(row, family).ok_or_else(invalid)?;
        let Some((coverage, count)) = sources.get_mut(row.source.as_str()) else { return Err(invalid()) };
        if !point.fits_source(source)
            || !matches!(coverage.state, SourceState::Collected | SourceState::Partial)
            || point.position() > coverage.records
            || !keys.insert((row.source.as_str(), row.key.as_str()))
            || original.is_some_and(|(id, context)| id != point.database_id() || context != point.context())
        {
            return Err(invalid());
        }
        original = Some((point.database_id(), point.context()));
        *count += 1;
        points.push((*row, point));
    }
    if sources.values().any(|(s, count)| *count > s.records as usize || read_complete && *count != s.records as usize) {
        return Err(invalid());
    }
    Ok(QualifiedCapture {
        context: original.map(|(_, context)| context.clone()).unwrap_or(fallback),
        sources: header.sources.clone(),
        points,
    })
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
pub(super) fn test_context() -> NativeReadContext {
    use culprit_core::{DatabaseIdentity, Engine};
    NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("member"),
            }),
            epoch: Some("original-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    }
}
