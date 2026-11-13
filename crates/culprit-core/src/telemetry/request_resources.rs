//! Current execution reservations are distinct from allocation classes, retained plan bytes and historical peaks.
//! A generation-checked request is an observed participant. Its current statement is context, not proof of which
//! statement acquired the reservation, and a waiting participant is affected rather than a memory owner.
use super::*;
use crate::MeasurementScope;
use std::collections::BTreeSet;

pub const MAX_REQUEST_MEMORY_OBSERVATIONS: usize = 4096;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ExecutionGrantState {
    Waiting,
    Granted,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RequestGrantAssociation {
    /// Native generations, request times and handles are consistent at observation; not a causal edge to waiters.
    GenerationChecked,
    MissingRequest,
    InsufficientEvidence,
    ChangedRequest,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GrantStatementRelationship {
    /// The current request's statement, which need not be the statement that acquired its reservation.
    ObservedCurrentStatement,
    Unresolved,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ExecutionGrantObservation {
    pub source: String,
    /// Includes the native grant request discriminator. Mutable counter values never establish identity.
    pub identity: NativeIdentity,
    pub observed_at: String,
    pub state: ExecutionGrantState,
    pub request_association: RequestGrantAssociation,
    pub statement_relationship: GrantStatementRelationship,
    pub request: Option<ActivityObservation>,
    /// None means native request/database ownership could not be established, not a database measurement of zero.
    pub scope: Option<MeasurementScope>,
    /// A source-local native time is not silently assigned a UTC offset.
    pub requested_without_offset: String,
    pub granted_without_offset: Option<String>,
    pub requested_kibibytes: Option<String>,
    pub required_kibibytes: Option<String>,
    pub granted_kibibytes: Option<String>,
    pub used_kibibytes: Option<String>,
    pub maximum_used_kibibytes: Option<String>,
    pub ideal_kibibytes: Option<String>,
    pub wait_ms: Option<String>,
    pub timeout_seconds: Option<String>,
    pub degree_of_parallelism: Option<String>,
    pub reserved_workers: Option<String>,
    pub used_workers: Option<String>,
    pub maximum_used_workers: Option<String>,
    /// Native resource identifiers, such as Resource Governor pools, are not cloud elastic-pool identities.
    pub resource: Option<NativeIdentity>,
    pub workload_group: Option<NativeIdentity>,
    /// Exact native identifiers and queue metadata can remain useful when they are insufficient to prove a shared
    /// resource link. These source-owned properties do not establish portable units or causal relationships.
    pub native_properties: std::collections::BTreeMap<String, String>,
    pub available_columns: Vec<String>,
    pub native_null_columns: Vec<String>,
}

fn local_time(value: &str) -> bool {
    value.len() <= 64 && chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
}

impl ExecutionGrantObservation {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.identity).map(|value| digest(&value)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut columns = BTreeSet::new();
        let mut nulls = BTreeSet::new();
        let associated = self.request_association == RequestGrantAssociation::GenerationChecked;
        valid_key(&self.source)
            && self.identity.is_valid()
            && self.resource.as_ref().is_none_or(NativeIdentity::is_valid)
            && self.workload_group.as_ref().is_none_or(NativeIdentity::is_valid)
            && native_properties_valid(&self.native_properties)
            && self.observed_at.len() <= 64
            && canonical_time(&self.observed_at).is_ok()
            && local_time(&self.requested_without_offset)
            && self.granted_without_offset.as_deref().is_none_or(local_time)
            && (self.state == ExecutionGrantState::Granted) == self.granted_without_offset.is_some()
            && associated == self.request.is_some()
            && associated == self.scope.is_some()
            && self.scope != Some(MeasurementScope::Unknown)
            && self.request.as_ref().is_none_or(|request| {
                request.is_valid()
                    && request.key.request.is_some()
                    && (self.scope != Some(MeasurementScope::Database)
                        || request.scope == ActivityScopeBasis::RequestDatabase)
            })
            && (self.statement_relationship != GrantStatementRelationship::ObservedCurrentStatement || associated)
            && [
                &self.requested_kibibytes,
                &self.required_kibibytes,
                &self.granted_kibibytes,
                &self.used_kibibytes,
                &self.maximum_used_kibibytes,
                &self.ideal_kibibytes,
                &self.wait_ms,
                &self.timeout_seconds,
                &self.degree_of_parallelism,
                &self.reserved_workers,
                &self.used_workers,
                &self.maximum_used_workers,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && self.available_columns.len() <= 64
            && self.available_columns.iter().all(|name| valid_key(name) && columns.insert(name))
            && self.native_null_columns.len() <= self.available_columns.len()
            && self.native_null_columns.iter().all(|name| columns.contains(name) && nulls.insert(name))
    }
    pub fn redact(&mut self, engine: crate::Engine) {
        if let Some(request) = &mut self.request {
            request.redact(engine);
        }
    }
}

#[cfg(test)]
mod tests;
