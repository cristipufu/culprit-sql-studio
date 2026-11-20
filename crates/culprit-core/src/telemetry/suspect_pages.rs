//! Editable native page history is a retained snapshot, never a current integrity assessment or event feed.
use super::*;
use crate::{Engine, SourceState};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlSuspectPageHistory {
    pub database: String,
    /// Local numeric selector, not a historical database incarnation or current replica identity.
    pub native_database_id: String,
    pub observed_at: String,
    pub database_writable: Option<bool>,
    /// Rows whose current native event_type is 1, 2 or 3; preserves the existing active-suspect population.
    pub count: String,
    /// All retained rows for the numeric database ID, including restored, repaired and deallocated statuses.
    pub retained_rows: String,
    /// MAX(last_update_date) over the 1/2/3 subset only; the native datetime has no established offset.
    pub last_reported_time_without_offset: Option<String>,
    pub reset: crate::ResetMarker,
}
impl MssqlSuspectPageHistory {
    pub fn active_count(&self) -> Option<u64> {
        native_count(&self.count)
    }
    pub fn retained_count(&self) -> Option<u64> {
        native_count(&self.retained_rows)
    }
    pub fn is_valid(&self) -> bool {
        !self.database.trim().is_empty()
            && self.database.len() <= 2048
            && native_count(&self.native_database_id).is_some_and(|id| id > 0 && id <= i32::MAX as u64)
            && self.active_count().zip(self.retained_count()).is_some_and(|(active, retained)| active <= retained)
            && self.observed_at.len() <= 64
            && ObservationBounds::new(&self.observed_at, &self.observed_at).is_some()
            && self
                .last_reported_time_without_offset
                .as_deref()
                .is_none_or(|at| at.len() <= 30 && super::catalog::mssql_datetime(at).is_some())
            && (self.count != "0" || self.last_reported_time_without_offset.is_none())
            && self.reset == crate::ResetMarker::Unknown
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.database, &self.native_database_id, &self.observed_at))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.is_valid()
            && context.target.database().is_some_and(|db| db.engine == Engine::Mssql && db.database == self.database)
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && context.database_writable.zip(self.database_writable).is_none_or(|(left, right)| left == right)
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let Ok(context) = suspect_page_context(source) else { return false };
        let SourceObservation::SuspectPages { native_database, .. } = &source.observation else { return false };
        self.native_database_id == native_database.id
            && self.database_writable == Some(true)
            && source.observation.snapshot().is_some_and(|snapshot| snapshot.sources[0].records == 1)
            && self.matches_context(&context)
    }
}

/// The ordinary scan and authenticated captured adapter feed one pure interpretation.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SuspectPageHistoryRead {
    pub target: CollectionTarget,
    pub context: ObservationReadContext,
    pub source: StateSourceCoverage,
    pub observation: Option<Box<MssqlSuspectPageHistory>>,
    pub read_complete: bool,
}
impl SuspectPageHistoryRead {
    pub fn is_valid(&self) -> bool {
        self.target.database().is_some_and(|db| db.engine == Engine::Mssql)
            && self.target.is_valid()
            && self.context.is_valid()
            && self.source.is_valid()
            && self.source.source == crate::source_id::SUSPECT_PAGES
            && self.source.records <= 1
            && self.source.state != SourceState::Empty
            && u32::from(self.observation.is_some()) <= self.source.records
            && (!self.read_complete || u32::from(self.observation.is_some()) == self.source.records)
            && self.context.native().is_none_or(|context| context.target == self.target)
            && self.observation.as_ref().is_none_or(|point| {
                matches!(self.source.state, SourceState::Collected | SourceState::Partial)
                    && point.is_valid()
                    && self.target.database().is_some_and(|db| db.database == point.database)
                    && self.context.native().is_none_or(|context| point.matches_context(context))
            })
    }
    pub fn from_source(
        source: &CollectionDescription,
        rows: &[&OperationalObservation],
        read_complete: bool,
    ) -> Result<Self> {
        let header = source.observation.snapshot().ok_or_else(invalid)?;
        let context = suspect_page_context(source)?;
        if header.dataset != SnapshotDataset::SuspectPages
            || !context.has_qualified_local_owner()
            || header.sources.len() != 1
            || header.sources[0].source != crate::source_id::SUSPECT_PAGES
        {
            return Err(invalid());
        }
        let observation = match rows {
            [] => None,
            [row] if row.is_valid() && row.source == crate::source_id::SUSPECT_PAGES => {
                let OperationalFact::SuspectPages(point) = &row.fact else { return Err(invalid()) };
                if !point.fits_source(source) {
                    return Err(invalid());
                }
                Some(point.clone())
            }
            _ => return Err(invalid()),
        };
        let read = Self {
            target: source.target.clone(),
            context: ObservationReadContext::Native { context: Box::new(context) },
            source: header.sources[0].clone(),
            observation,
            read_complete,
        };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
}
/// The current selected native ID and process are acquisition provenance, not the history row's incarnation.
pub fn suspect_pages_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::SuspectPages
        && snapshot.records <= 1
        && matches!(snapshot.sources.as_slice(), [source] if source.source == crate::source_id::SUSPECT_PAGES && source.records == snapshot.records && source.state != SourceState::Empty)
}
pub fn suspect_page_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::SuspectPages { snapshot, native_database, database_writable } = &source.observation else {
        return Err(invalid());
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if !suspect_pages_header_is_valid(snapshot)
        || !validate_database_performance_identity(native_database)
        || !context.has_qualified_local_owner()
        || context.target.engine() != Engine::Mssql
        || !*database_writable
        || !matches!(&context.partition,SourcePartition::InstanceLocal{member:Some(member),epoch:Some(epoch)} if member.kind==SharedResourceKind::Instance && native_database.key().ok().as_ref()==Some(epoch))
    {
        return Err(invalid());
    }
    Ok(context)
}

fn native_count(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| *number <= i64::MAX as u64 && number.to_string() == value)
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
