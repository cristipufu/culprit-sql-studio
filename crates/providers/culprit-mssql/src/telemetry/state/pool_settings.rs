//! Closed pool Settings profile over StateSource's existing observation and finalization owner.
use super::super::subject::Identity;
use culprit_core::{Result, telemetry::*};

const MAX_TOTAL_QUERIES: u32 = 9;
const PAGE_BYTES: u32 = 16 * 1024;
const TOTAL_BYTES: u32 = 32 * 1024;

pub(super) fn selected(request: &CollectionRequest) -> bool {
    request.selection == CollectionSelection::ResourcePoolSettings
}

pub(super) fn dataset(selection: &CollectionSelection) -> Result<SnapshotDataset> {
    match selection {
        CollectionSelection::ResourcePoolSettings => Ok(SnapshotDataset::Settings),
        CollectionSelection::Snapshot { dataset } => Ok(*dataset),
        _ => Err(crate::read::invalid_data()),
    }
}

pub(super) async fn read_subject(
    reader: &mut impl crate::read::NativeRead,
    subject: &CollectionSubject,
    host: &str,
    port: u16,
) -> Result<super::super::subject::SubjectRead> {
    super::super::description::ensure_supported(reader).await?;
    Ok(super::super::subject::SubjectRead {
        identity: Identity::ResourcePoolSettings(Box::new(
            crate::resources::pool::SettingsContext::read(reader, subject, host, port).await?,
        )),
        configured_log_rates: Ok(None),
    })
}

pub(in crate::telemetry) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    // SET1 + Azure profile1 + initial member1 + clocks2 + pressure1 + layout1 + point1 + final member1.
    caller.intersection(&CollectionBudget {
        max_queries: MAX_TOTAL_QUERIES,
        max_snapshot_rows: MSSQL_POOL_SETTING_NAMES.len() as u32,
        page_rows: MSSQL_POOL_SETTING_NAMES.len() as u32,
        page_bytes: PAGE_BYTES,
        total_bytes: TOTAL_BYTES,
        max_pages: MSSQL_POOL_SETTING_NAMES.len() as u32,
        statement_timeout_ms: 1_000,
        total_timeout_ms: 5_000,
        ..CollectionBudget::upper_limits()
    })
}

pub(super) fn observation(
    snapshot: StateSnapshotDescription,
    identity: &Identity,
    point: Option<MssqlPoolSettingsPoint>,
) -> Result<SourceObservation> {
    if snapshot.dataset != SnapshotDataset::Settings || !matches!(identity, Identity::ResourcePoolSettings(_)) {
        return super::local_point::observation(snapshot, identity);
    }
    let Identity::ResourcePoolSettings(owner) = identity else { return Err(crate::read::invalid_data()) };
    let membership = owner.settings_membership(point);
    if !membership.matches_target(&identity.target()) || !membership.matches_snapshot(&snapshot) {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(SourceObservation::MssqlPoolSettings { snapshot, membership: Box::new(membership) })
}

pub(super) fn confirmed_by(source: &CollectionDescription, identity: &Identity) -> bool {
    let SourceObservation::MssqlPoolSettings { snapshot, membership } = &source.observation else { return true };
    let Identity::ResourcePoolSettings(after) = identity else { return false };
    let observed_after = after.settings_membership(None);
    observed_after.member == membership.member
        && observed_after.database == membership.database
        && observed_after.elastic_pool == membership.elastic_pool
        && observed_after.elastic_pool_error == membership.elastic_pool_error
        && observed_after.database_writable == membership.database_writable
        && ObservationBounds::new(&snapshot.observed_until, after.observed_at()).is_some()
}

#[cfg(test)]
mod tests;
