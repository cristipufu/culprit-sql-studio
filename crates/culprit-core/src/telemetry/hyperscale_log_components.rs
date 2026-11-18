//! Native component reports are separate limits, rates and unapplied-log distances, never throughput or cause.
use super::*;
use std::collections::BTreeSet;

pub const HYPERSCALE_LOG_COMPONENTS_SOURCE: &str = "hyperscale-log-components";
pub const MAX_HYPERSCALE_LOG_COMPONENT_ROWS: u32 = 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct HyperscaleLogComponent {
    pub database_id: String,
    pub database_incarnation: String,
    pub replica_id: Option<String>,
    pub file_id: Option<String>,
    pub component_id: String,
    /// Native tinyint; future/unknown roles stay numeric and never establish membership.
    pub role: u8,
    pub role_desc: Option<String>,
    pub current_max_log_rate_bps: String,
    pub catchup_rate_bps: String,
    pub catchup_distance_bytes: String,
    /// Native datetime has no offset. It is not compared with acquisition UTC or a wait incident.
    pub last_reported_time_without_offset: Option<String>,
}
impl HyperscaleLogComponent {
    pub fn is_valid(&self) -> bool {
        integer(&self.database_id, i32::MAX as u64).is_some_and(|id| id > 0)
            && valid_key(&self.database_incarnation)
            && guid(&self.component_id)
            && self.replica_id.as_deref().is_none_or(guid)
            && self.file_id.as_deref().is_none_or(|id| integer(id, i32::MAX as u64).is_some_and(|id| id > 0))
            && self
                .role_desc
                .as_deref()
                .is_none_or(|name| name.encode_utf16().count() <= 60 && !name.chars().any(char::is_control))
            && [&self.current_max_log_rate_bps, &self.catchup_rate_bps, &self.catchup_distance_bytes]
                .into_iter()
                .all(|value| integer(value, i64::MAX as u64).is_some())
            && self.last_reported_time_without_offset.as_deref().is_none_or(|at| {
                at.len() <= 23
                    && super::catalog::local_time(at)
                    && chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S%.f")
                        .is_ok_and(|at| (1753..=9999).contains(&chrono::Datelike::year(&at)))
            })
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.database_incarnation, &self.database_id, &self.component_id.to_ascii_lowercase()))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        self.is_valid()
            && source.target.engine() == crate::Engine::Mssql
            && source.observation.collection()
                == TelemetryCollection::Snapshot(SnapshotDataset::HyperscaleLogComponents)
            && source
                .target
                .database()
                .is_some_and(|database| database.incarnation.as_deref() == Some(self.database_incarnation.as_str()))
    }
}

/// Preserve the original logical namespace and its independently declared execution context once.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct HyperscaleLogComponentRead {
    pub description: Box<CollectionDescription>,
    pub observations: Vec<OperationalObservation>,
    pub read_complete: bool,
}
impl HyperscaleLogComponentRead {
    pub fn from_source(
        source: &CollectionDescription,
        observations: Vec<OperationalObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let read = Self { description: Box::new(source.clone()), observations, read_complete };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
    pub fn context(&self) -> Result<NativeReadContext> {
        NativeReadContext::from_snapshot(&self.description)
    }
    pub fn coverage(&self) -> Result<&StateSourceCoverage> {
        let header = self
            .description
            .observation
            .snapshot()
            .filter(|header| header.dataset == SnapshotDataset::HyperscaleLogComponents)
            .ok_or_else(invalid)?;
        let [coverage] = header.sources.as_slice() else {
            return Err(invalid());
        };
        if coverage.source != HYPERSCALE_LOG_COMPONENTS_SOURCE {
            return Err(invalid());
        }
        Ok(coverage)
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        let Ok(context) = self.context() else {
            return false;
        };
        let Ok(source) = self.coverage() else {
            return false;
        };
        context.target.engine() == crate::Engine::Mssql
            && context.target.database().is_some()
            && source.is_valid()
            && source.records <= MAX_HYPERSCALE_LOG_COMPONENT_ROWS
            && self.observations.len() <= source.records as usize
            && (!self.read_complete || self.observations.len() == source.records as usize)
            && self.observations.iter().all(|row| {
                let OperationalFact::HyperscaleLogComponent(point) = &row.fact else {
                    return false;
                };
                row.is_valid()
                    && row.source == source.source
                    && keys.insert(&row.key)
                    && matches!(source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                    && point.fits_source(&self.description)
            })
    }
}
fn integer(value: &str, maximum: u64) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| *number <= maximum && number.to_string() == value)
}
fn guid(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .enumerate()
            .all(|(at, byte)| if [8, 13, 18, 23].contains(&at) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
