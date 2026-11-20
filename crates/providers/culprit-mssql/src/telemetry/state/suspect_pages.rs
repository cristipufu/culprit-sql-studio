//! The explicitly selected boxed source reuses StateSource's same retained native owner and point budget.
use super::*;
pub(in crate::telemetry) async fn read<R: NativeRead>(
    reader: &mut R,
    owner: &crate::local_source::Context,
) -> Result<point::ReadOutcome> {
    let generation = reader.connection_generation();
    if !owner.is_qualified() || generation.is_none() || generation != owner.connection_generation() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let facts = crate::operations::native::read(reader, crate::operations::native::Kind::SuspectPages, 1).await?;
    let crate::operations::native::Facts::SuspectPages(point) = &facts else { return Err(invalid_data()) };
    if reader.connection_generation() != generation
        || point.native_database_id != owner.database_id.to_string()
        || point.database != owner.activity.database.database
        || point.database_writable != owner.activity.writable
        || point.database_writable != Some(true)
        || ObservationBounds::new(&owner.activity.at, &point.observed_at).is_none()
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(point::ReadOutcome::Observed(point::Records {
        values: facts
            .observations(crate::operations::native::Kind::SuspectPages)?
            .into_iter()
            .map(TelemetryRecord::Operational)
            .collect(),
        // Complete native table snapshot only. The pure view separately withholds historical completeness.
        complete_scope: true,
        error_code: None,
    }))
}

#[cfg(test)]
mod tests;
