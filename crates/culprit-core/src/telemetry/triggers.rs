//! Native DML trigger metadata. Unknown event properties never become observed false values.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlDmlTrigger {
    pub identity: MssqlObjectIdentity,
    pub parent: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub parent_schema: String,
    pub parent_name: String,
    /// Native U (table) or V (view), and TR (SQL) or TA (CLR).
    pub parent_type: String,
    pub native_type: String,
    pub modified_without_offset: String,
    pub disabled: bool,
    pub instead_of: bool,
    pub not_for_replication: bool,
    pub insert_event: Option<bool>,
    pub delete_event: Option<bool>,
    pub update_event: Option<bool>,
}
impl MssqlDmlTrigger {
    pub fn event_metadata_complete(&self) -> bool {
        [self.insert_event, self.delete_event, self.update_event].iter().all(Option::is_some)
    }
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && self.parent.is_valid()
            && self.identity.database_id == self.parent.database_id
            && self.identity.object_id != self.parent.object_id
            && [&self.schema, &self.name, &self.parent_schema, &self.parent_name]
                .iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && matches!(self.parent_type.as_str(), "U" | "V")
            && matches!(self.native_type.as_str(), "TR" | "TA")
            && super::catalog::local_time(&self.modified_without_offset)
            && (!self.event_metadata_complete()
                || [self.insert_event, self.delete_event, self.update_event].contains(&Some(true)))
    }
}
