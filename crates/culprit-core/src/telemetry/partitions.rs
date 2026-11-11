//! Partition membership and boundary direction are native facts; converted display text is not a lossless value.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlPartitionBoundary {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    /// Zero identifies a partitioned heap. Native index IDs do not establish a counter reset generation.
    pub index_id: u32,
    pub index_name: Option<String>,
    pub index_disabled: bool,
    pub index_hypothetical: bool,
    pub scheme_id: u32,
    pub scheme_name: String,
    pub function_id: u32,
    pub function_name: String,
    pub function_created_without_offset: String,
    pub function_modified_without_offset: String,
    pub fanout: u32,
    pub boundary_on_right: bool,
    pub parameter_id: u16,
    pub parameter_type_schema: String,
    pub parameter_type: String,
    pub parameter_system_type_id: u16,
    pub parameter_user_type_id: u32,
    pub parameter_maximum_bytes: u16,
    pub parameter_precision: u8,
    pub parameter_scale: u8,
    pub parameter_collation: Option<String>,
    /// None is an observed function with no boundaries. A native SQL NULL boundary has an ID and value_is_null=true.
    pub boundary_id: Option<u32>,
    pub value_is_null: Option<bool>,
    pub native_value_type: Option<String>,
    /// SHA-256 of SQL Server's native binary conversion; opaque and qualified by the recorded parameter type.
    /// Missing when no value exists or the native binary representation exceeds the hash input budget.
    pub native_value_sha256: Option<String>,
    /// SQL Server's converted diagnostic text, redacted with the existing partition-date policy.
    /// Native fingerprints detect changes hidden by display rounding or redaction.
    pub display_value: Option<CatalogExpression>,
}
impl MssqlPartitionBoundary {
    pub fn key(&self) -> crate::Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&(
            &self.table,
            self.index_id,
            self.function_id,
            &self.function_created_without_offset,
            self.parameter_id,
            self.boundary_id,
        ))
        .map(|bytes| digest(&bytes))
        .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let name = |name: &str| !name.is_empty() && name.len() <= 2048;
        self.table.is_valid()
            && [
                &self.schema,
                &self.table_name,
                &self.scheme_name,
                &self.function_name,
                &self.parameter_type_schema,
                &self.parameter_type,
            ]
            .iter()
            .all(|value| name(value))
            && self.index_name.as_deref().is_none_or(name)
            && (self.index_id == 0) == self.index_name.is_none()
            && self.scheme_id > 0
            && self.function_id > 0
            && self.fanout > 0
            && self.parameter_id > 0
            && self.parameter_system_type_id > 0
            && self.parameter_user_type_id > 0
            && self.parameter_maximum_bytes > 0
            && self.parameter_collation.as_deref().is_none_or(name)
            && super::catalog::local_time(&self.function_created_without_offset)
            && super::catalog::local_time(&self.function_modified_without_offset)
            && self.boundary_id.is_none_or(|id| id > 0 && id < self.fanout)
            && self.boundary_id.is_some() == self.value_is_null.is_some()
            && self.boundary_id.is_some() == self.display_value.is_some()
            && self.native_value_sha256.as_deref().is_none_or(valid_digest)
            && (self.value_is_null == Some(false) || self.native_value_sha256.is_none())
            && self.native_value_type.as_deref().is_none_or(name)
            && (self.value_is_null == Some(false) || self.native_value_type.is_none())
            && self.display_value.as_ref().is_none_or(CatalogExpression::is_valid)
            && (self.boundary_id.is_some() || self.fanout == 1)
    }
}
