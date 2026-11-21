//! Current pool-native visibility and anchor membership. Neither a pool incarnation nor historical membership.
use super::*;

pub const MSSQL_POOL_CURRENT_SOURCE: &str = "azure-pool-wait-counter-state";

/// These native points bracket one current read; engine startup/reset and historical row ownership stay unknown.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPoolCurrentPoint {
    pub pool: SharedResourceIdentity,
    pub member: NativeIdentity,
    /// Pool-local DMV database_id; never a sys.database_service_objectives catalog ID.
    pub database_id: u32,
    pub database_name: String,
    pub engine_edition: u32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_tier: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub server_performance_state: Option<bool>,
    pub database_writable: bool,
    pub observed_at: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MssqlPoolScopeQualification {
    Qualified,
    UnqualifiedTier,
    VisibilityUnavailable,
}
impl MssqlPoolCurrentPoint {
    pub fn is_valid(&self) -> bool {
        self.engine_edition == 5
            && self.pool.is_valid()
            && self.pool.engine == crate::Engine::Mssql
            && self.pool.kind == SharedResourceKind::Pool
            && self.pool.namespace.id == self.member.id
            && self.pool.namespace.qualifiers.len() == 1
            && self.pool.namespace.qualifiers.get("kind").map(String::as_str) == Some("azure-sql-logical-server")
            && self.pool.native.qualifiers.is_empty()
            && self.member.is_valid()
            && self.member.qualifiers.len() == 2
            && ["logical_database_guid", "physical_database_guid"].into_iter().all(|name| {
                self.member.qualifiers.get(name).is_some_and(|guid| {
                    replication::native_guid(guid) && guid.bytes().any(|byte| byte != b'0' && byte != b'-')
                })
            })
            && self.database_id > 0
            && self.database_id <= i32::MAX as u32
            && !self.database_name.trim().is_empty()
            && self.database_name.encode_utf16().count() <= 128
            && !self.database_name.chars().any(char::is_control)
            && self
                .native_tier
                .as_ref()
                .is_none_or(|tier| !tier.trim().is_empty() && tier.len() <= 128 && !tier.chars().any(char::is_control))
            && self.database_writable
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
    }
    /// Native protocol labels, never translated wording or an EngineEdition-only scope guess.
    pub fn scope_qualification(&self) -> MssqlPoolScopeQualification {
        if !matches!(
            self.native_tier.as_deref(),
            Some("General Purpose" | "Business Critical" | "Basic" | "Standard" | "Premium")
        ) {
            return MssqlPoolScopeQualification::UnqualifiedTier;
        }
        if self.server_performance_state != Some(true) {
            return MssqlPoolScopeQualification::VisibilityUnavailable;
        }
        MssqlPoolScopeQualification::Qualified
    }
    pub fn same_membership(&self, other: &Self) -> bool {
        self.is_valid()
            && other.is_valid()
            && self.pool == other.pool
            && self.member == other.member
            && self.database_id == other.database_id
            && self.database_name == other.database_name
            && self.engine_edition == other.engine_edition
            && self.native_tier == other.native_tier
            && self.server_performance_state == other.server_performance_state
            && self.database_writable == other.database_writable
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPoolCurrentAuthority {
    pub before: MssqlPoolCurrentPoint,
    pub after: MssqlPoolCurrentPoint,
}
impl MssqlPoolCurrentAuthority {
    pub fn is_valid(&self) -> bool {
        self.before.same_membership(&self.after)
            && ObservationBounds::new(&self.before.observed_at, &self.after.observed_at).is_some()
    }
    pub fn accepts_sources(&self, sources: &[StateSourceCoverage]) -> bool {
        sources.len() == 1
            && sources[0].is_valid()
            && sources[0].source == crate::source_id::WAITS
            && (self.before.scope_qualification() == MssqlPoolScopeQualification::Qualified
                || sources[0].records == 0
                    && sources[0].state == crate::SourceState::Unavailable
                    && !sources[0].complete_scope)
    }
    pub fn matches_snapshot(&self, snapshot: &StateSnapshotDescription) -> bool {
        if !self.is_valid()
            || !snapshot.is_valid()
            || snapshot.dataset != SnapshotDataset::WaitCounters
            || !self.accepts_sources(&snapshot.sources)
            || ObservationBounds::new(&self.before.observed_at, &snapshot.observed_from).is_none()
            || canonical_time(&self.after.observed_at).ok() != canonical_time(&snapshot.observed_until).ok()
        {
            return false;
        }
        self.before.scope_qualification() == MssqlPoolScopeQualification::Qualified
            || snapshot.records == 0
                && snapshot.sources[0].records == 0
                && snapshot.sources[0].state == crate::SourceState::Unavailable
                && !snapshot.sources[0].complete_scope
    }
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.is_valid()
            && context.partition == SourcePartition::Target
            && context.target == CollectionTarget::Shared(self.before.pool.clone())
            && context.database_writable == Some(true)
            && ObservationBounds::new(&self.before.observed_at, &context.observed_from).is_some()
            && canonical_time(&self.after.observed_at).ok() == canonical_time(&context.observed_until).ok()
    }
}
pub fn mssql_pool_current_authority(source: &CollectionDescription) -> Result<Option<&MssqlPoolCurrentAuthority>> {
    let SourceObservation::MssqlPoolCurrent { snapshot, authority } = &source.observation else { return Ok(None) };
    if source.source != MSSQL_POOL_CURRENT_SOURCE
        || source.partition != SourcePartition::Target
        || source.continuity != SourceContinuity::Unverified
        || source.target != CollectionTarget::Shared(authority.before.pool.clone())
        || !authority.matches_snapshot(snapshot)
    {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(Some(authority))
}
impl WaitCounterObservation {
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let authority = match mssql_pool_current_authority(source) {
            Ok(Some(authority)) => authority,
            Ok(None) => {
                return !(matches!(&source.target, CollectionTarget::Shared(pool) if pool.engine == crate::Engine::Mssql && pool.kind == SharedResourceKind::Pool)
                    || source.target.engine() == crate::Engine::Mssql && self.scope == crate::MeasurementScope::Pool);
            }
            Err(_) => return false,
        };
        authority.before.scope_qualification() == MssqlPoolScopeQualification::Qualified
            && self.is_valid_for(crate::Engine::Mssql)
            && self.source == crate::source_id::WAITS
            && self.scope == crate::MeasurementScope::Pool
            && self.reset == crate::ResetMarker::Unknown
    }
}
#[cfg(test)]
mod tests;
