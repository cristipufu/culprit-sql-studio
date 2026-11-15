//! Read ownership does not establish the execution origin or lifetime represented by retained native counters.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeReadContext {
    pub target: CollectionTarget,
    pub partition: SourcePartition,
    /// Bounds of the retained native observations. They are not counter lifetimes or acquisition-cost measurements.
    pub observed_from: String,
    pub observed_until: String,
    /// Native updateability observed on this connection. It grants no permission to execute a change.
    pub database_writable: Option<bool>,
}

impl NativeReadContext {
    pub fn from_snapshot(source: &CollectionDescription) -> Result<Self> {
        let snapshot = source.observation.snapshot().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if !source.partition_is_consistent() || !snapshot.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let context = Self {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: None,
        };
        if !context.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(context)
    }

    pub fn is_valid(&self) -> bool {
        self.target.is_valid()
            && self.partition.is_valid_for(&self.target)
            && self.observed_from.len() <= 64
            && self.observed_until.len() <= 64
            && ObservationBounds::new(&self.observed_from, &self.observed_until).is_some()
    }

    pub fn has_qualified_local_owner(&self) -> bool {
        self.is_valid()
            && matches!(self.partition, SourcePartition::InstanceLocal { .. })
            && self.partition.is_qualified()
    }
}

#[cfg(test)]
pub(super) mod tests;
