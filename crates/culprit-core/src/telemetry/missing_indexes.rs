//! Native optimizer advice is a point observation with unknown counter continuity and optional query associations.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlMissingIndexSnapshot {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    pub index_handle: u32,
    pub group_handle: u32,
    pub equality: Vec<String>,
    pub inequality: Vec<String>,
    pub include: Vec<String>,
    pub seeks: String,
    pub scans: String,
    /// Optimizer cost units, not measured CPU time or duration.
    pub average_query_cost: f64,
    pub estimated_impact_percent: f64,
    pub last_seek_without_offset: Option<String>,
    pub last_scan_without_offset: Option<String>,
    pub reset: crate::ResetMarker,
    /// None means associations were unavailable. An empty list is a completed native association read.
    pub query_hashes: Option<Vec<String>>,
}
impl MssqlMissingIndexSnapshot {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.table, self.index_handle, self.group_handle))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && [&self.schema, &self.table_name].into_iter().all(|value| !value.is_empty() && value.len() <= 2048)
            && [&self.equality, &self.inequality, &self.include]
                .into_iter()
                .all(|names| names.len() <= 1024 && names.iter().all(|name| !name.is_empty() && name.len() <= 2048))
            && (!self.equality.is_empty() || !self.inequality.is_empty())
            && [&self.seeks, &self.scans].into_iter().all(|value| valid_counter(value))
            && self.average_query_cost.is_finite()
            && self.average_query_cost >= 0.0
            && self.estimated_impact_percent.is_finite()
            && (0.0..=100.0).contains(&self.estimated_impact_percent)
            && [&self.last_seek_without_offset, &self.last_scan_without_offset]
                .into_iter()
                .flatten()
                .all(|at| super::catalog::local_time(at))
            && valid_reset(&self.reset)
            && self.query_hashes.as_ref().is_none_or(|hashes| {
                hashes.len() <= 10_000
                    && hashes.iter().all(|hash| {
                        hash.strip_prefix("0x")
                            .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    })
            })
    }
}
