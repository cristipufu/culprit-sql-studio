//! Statistics properties are observations, not a guarantee that every catalog entry has a readable blob.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlStatisticsSnapshot {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    pub statistics_id: u32,
    pub name: String,
    pub auto_created: bool,
    pub user_created: bool,
    pub no_recompute: bool,
    pub memory_optimized: bool,
    pub properties_available: bool,
    pub last_updated_without_offset: Option<String>,
    pub rows: Option<String>,
    pub rows_sampled: Option<String>,
    pub histogram_steps: Option<u32>,
    pub unfiltered_rows: Option<String>,
    /// Modifications of the leading column; on memory-optimized tables the native counter covers the table.
    pub modifications: Option<String>,
    /// A local statistics-update time alone cannot establish a UTC epoch or exclude other native resets.
    pub modifications_reset: crate::ResetMarker,
}
impl MssqlStatisticsSnapshot {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.table, self.statistics_id))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let counters = [&self.rows, &self.rows_sampled, &self.unfiltered_rows, &self.modifications];
        self.table.is_valid()
            && self.statistics_id > 0
            && [&self.schema, &self.table_name, &self.name]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && counters.into_iter().flatten().all(|value| valid_counter(value))
            && self.last_updated_without_offset.as_deref().is_none_or(super::catalog::local_time)
            && valid_reset(&self.modifications_reset)
            && (self.properties_available
                || (counters.into_iter().all(Option::is_none)
                    && self.last_updated_without_offset.is_none()
                    && self.histogram_steps.is_none()))
    }
}
