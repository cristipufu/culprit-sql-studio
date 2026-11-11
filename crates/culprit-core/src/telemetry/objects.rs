//! Native catalog dates are metadata observations; they do not establish a UTC change event or a removal.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCatalogObject {
    pub identity: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub native_type: String,
    pub modified_without_offset: String,
    /// A missing parent means the native parent_object_id is zero.
    pub parent_object_id: Option<u32>,
    pub parent_schema: Option<String>,
    pub parent_name: Option<String>,
}
impl MssqlCatalogObject {
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && [&self.schema, &self.name].into_iter().all(|name| !name.is_empty() && name.len() <= 2048)
            && matches!(self.native_type.as_str(), "U" | "V" | "P" | "FN" | "IF" | "TF" | "TR")
            && super::catalog::local_time(&self.modified_without_offset)
            && self.parent_object_id.is_none_or(|id| id > 0)
            && [&self.parent_schema, &self.parent_name]
                .into_iter()
                .flatten()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && (self.parent_object_id.is_some() || self.parent_schema.is_none() && self.parent_name.is_none())
    }
}
