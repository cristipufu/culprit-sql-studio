//! Observed cleanup retainers keep native horizons and lifecycle context separate from causal claims.
use super::*;
use crate::{CleanupHolder, MeasurementScope, OpenTransaction, SourceState, TransactionActivity};
use std::collections::BTreeSet;

pub const MAX_RETENTION_HOLDERS: usize = 4096;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CleanupHorizon {
    Data,
    /// Non-shared catalogs of the selected database.
    Catalog,
    /// Catalogs shared across databases; database-filtered session views cannot establish this boundary.
    SharedCatalog,
}

/// A native horizon and its transaction distance, not a count of rows or bytes it prevents reclaiming.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetentionHorizon {
    pub protects: Vec<CleanupHorizon>,
    pub native_name: String,
    pub native_value: String,
    pub transactions_behind: Option<String>,
}
impl RetentionHorizon {
    fn is_valid(&self) -> bool {
        let mut protections = BTreeSet::new();
        !self.protects.is_empty()
            && self.protects.len() <= 3
            && self.protects.iter().all(|horizon| protections.insert(*horizon))
            && valid_key(&self.native_name)
            && valid_key(&self.native_value)
            && self.transactions_behind.as_deref().is_none_or(valid_counter)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RetentionQueryRelationship {
    ExecutingAtObservation,
    PreviousStatement,
}

/// A statement observed in the retaining session. Neither relationship proves which code opened the transaction.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetentionQueryContext {
    pub identity: Option<NativeQueryIdentity>,
    pub started_at: Option<String>,
    pub relationship: RetentionQueryRelationship,
}
impl RetentionQueryContext {
    fn is_valid(&self) -> bool {
        (self.identity.is_some() || self.started_at.is_some())
            && self.identity.as_ref().is_none_or(NativeQueryIdentity::is_valid)
            && self.started_at.as_deref().is_none_or(valid_time)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetentionTransactionContext {
    pub native_id: Option<String>,
    pub started_at: Option<String>,
    /// Preparation time is not an invented transaction begin time.
    pub prepared_at: Option<String>,
}
impl RetentionTransactionContext {
    fn is_valid(&self) -> bool {
        (self.native_id.is_some() || self.started_at.is_some() || self.prepared_at.is_some())
            && self.native_id.as_deref().is_none_or(valid_key)
            && [&self.started_at, &self.prepared_at].into_iter().flatten().all(|at| valid_time(at))
    }
}

/// One current native retainer. Slot names without a creation generation never establish historical continuity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RetentionObservation {
    pub source: String,
    pub identity: NativeIdentity,
    pub observed_at: String,
    pub scope: MeasurementScope,
    pub holder: CleanupHolder,
    pub name: String,
    pub horizons: Vec<RetentionHorizon>,
    pub session: Option<NativeActivityIdentity>,
    /// A native PID without a visible generation cannot be joined to a sampled application session.
    pub unresolved_session_id: Option<String>,
    pub transaction: Option<RetentionTransactionContext>,
    pub query: Option<RetentionQueryContext>,
    pub client: Option<ActivityClient>,
    pub activity: TransactionActivity,
    pub native_state: Option<String>,
    pub age_seconds: Option<f64>,
    pub database: Option<String>,
    /// Application code ownership is not established by a login, application name or previous SQL statement.
    pub code_ownership: PayloadUnavailable,
}
fn valid_time(at: &str) -> bool {
    at.len() <= 64 && canonical_time(at).is_ok()
}
impl RetentionObservation {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&self.identity).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let text = |value: &String| value.len() <= 2048 && !value.chars().any(char::is_control);
        let mut names = BTreeSet::new();
        valid_key(&self.source)
            && self.identity.is_valid()
            && self.scope != MeasurementScope::Unknown
            && valid_time(&self.observed_at)
            && text(&self.name)
            && !self.name.is_empty()
            && !self.horizons.is_empty()
            && self.horizons.len() <= 8
            && self.horizons.iter().all(|h| h.is_valid() && names.insert(&h.native_name))
            && self.session.as_ref().is_none_or(NativeActivityIdentity::is_valid)
            && self.unresolved_session_id.as_deref().is_none_or(valid_key)
            && (self.session.is_none() || self.unresolved_session_id.is_none())
            && self.transaction.as_ref().is_none_or(RetentionTransactionContext::is_valid)
            && self.query.as_ref().is_none_or(RetentionQueryContext::is_valid)
            && (self.query.is_none() || self.session.is_some())
            && self.client.as_ref().is_none_or(ActivityClient::is_valid)
            && [&self.native_state, &self.database].into_iter().flatten().all(text)
            && self.age_seconds.is_none_or(|age| age.is_finite() && age >= 0.0)
    }
    pub fn transactions_behind(&self, kind: CleanupHorizon) -> Option<u64> {
        self.horizons
            .iter()
            .filter(|horizon| horizon.protects.contains(&kind))
            .filter_map(|horizon| horizon.transactions_behind.as_deref()?.parse().ok())
            .max()
    }
    /// Existing presentation vocabulary, projected from a selected native horizon; never used as a fallback read.
    pub fn display(&self, kind: CleanupHorizon) -> OpenTransaction {
        OpenTransaction {
            catalog_only: kind != CleanupHorizon::Data,
            session: self.name.clone(),
            seconds: self.age_seconds.unwrap_or(0.0),
            program: self.client.as_ref().and_then(|client| client.application.clone().or_else(|| client.user.clone())),
            activity: self.activity,
            state: self.native_state.clone(),
            holder: self.holder,
            xid_age: self.transactions_behind(kind).map(|age| age as f64),
            database: self.database.clone(),
            ..Default::default()
        }
    }
    pub(crate) fn redact_context(&mut self) {
        self.client = None;
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetentionSourceCoverage {
    pub source: String,
    pub state: SourceState,
}

/// Native profile qualification, carried even when no holder rows exist. Read completeness still comes from the
/// authenticated capture's source outcomes; this metadata alone can never establish a complete boundary.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetentionCoverage {
    pub native_sources: Vec<String>,
    pub qualified_horizons: Vec<CleanupHorizon>,
}
impl RetentionCoverage {
    pub const SOURCE: &str = "cleanup-horizon-coverage";
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        let mut sources = BTreeSet::new();
        let mut horizons = BTreeSet::new();
        !self.native_sources.is_empty()
            && self.native_sources.len() <= 16
            && self
                .native_sources
                .iter()
                .all(|source| valid_key(source) && source != Self::SOURCE && sources.insert(source))
            && self.qualified_horizons.len() <= 3
            && self.qualified_horizons.iter().all(|horizon| horizons.insert(*horizon))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RetentionSnapshot {
    pub holders: Vec<RetentionObservation>,
    pub sources: Vec<RetentionSourceCoverage>,
    pub coverage: RetentionCoverage,
}
impl RetentionSnapshot {
    pub fn is_valid(&self) -> bool {
        let mut sources = BTreeSet::new();
        let mut identities = BTreeSet::new();
        !self.sources.is_empty()
            && self.coverage.is_valid()
            && self.sources.len() <= 16
            && self.sources.iter().all(|source| valid_key(&source.source) && sources.insert(source.source.as_str()))
            && sources == self.coverage.native_sources.iter().map(String::as_str).collect()
            && self.holders.len() <= MAX_RETENTION_HOLDERS
            && self.holders.iter().all(|holder| {
                holder.is_valid()
                    && sources.contains(holder.source.as_str())
                    && holder.key().is_ok_and(|key| identities.insert(key))
            })
            && self.sources.iter().all(|source| {
                let rows = self.holders.iter().any(|holder| holder.source == source.source);
                match source.state {
                    SourceState::Collected => rows,
                    SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable => !rows,
                    SourceState::Partial => true,
                }
            })
    }
    /// Source visibility and native boundary qualification are both necessary. A complete database read does not
    /// establish the shared-catalog boundary across databases.
    pub fn is_horizon_complete(&self, horizon: CleanupHorizon) -> bool {
        self.is_valid()
            && self.coverage.qualified_horizons.contains(&horizon)
            && self.sources.iter().all(|source| matches!(source.state, SourceState::Collected | SourceState::Empty))
    }
    /// Select each horizon independently and preserve ties. Partial reads never establish a unique global oldest.
    pub fn oldest_observed(&self, kind: CleanupHorizon) -> Vec<&RetentionObservation> {
        if !self.is_valid() {
            return Vec::new();
        }
        let oldest = self.holders.iter().filter_map(|holder| holder.transactions_behind(kind)).max();
        self.holders.iter().filter(|holder| oldest.is_some() && holder.transactions_behind(kind) == oldest).collect()
    }
}

#[cfg(test)]
mod tests;
