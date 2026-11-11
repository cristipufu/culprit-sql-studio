use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const ROOT_KEY: &str = "recovery.json";
pub(super) const MAX_ROOT_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_INDEX_BYTES: usize = 2048;
pub(super) const MAX_RECOVERY_PAGE: u32 = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitIndex {
    pub store_id: String,
    pub order: u64,
    pub range_id: String,
    pub manifest_digest: String,
    pub high_sequence: u64,
    pub previous: Option<String>,
}
impl CommitIndex {
    pub fn validate(&self, store_id: &str) -> Result<()> {
        if self.store_id != store_id
            || self.order == 0
            || self.high_sequence == 0
            || !valid_digest(&self.range_id)
            || !valid_digest(&self.manifest_digest)
            || self.previous.as_ref().is_some_and(|id| !valid_digest(id))
            || (self.order == 1) != self.previous.is_none()
        {
            return Err(integrity());
        }
        Ok(())
    }
    pub fn range_key(&self) -> String {
        format!("ranges/{}.json", self.range_id)
    }
    pub fn order_key(&self) -> String {
        format!("commits/{:020}.json", self.order)
    }
    pub fn manifest_key(&self) -> String {
        format!("manifests/{}.json", self.manifest_digest)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Root {
    pub store_id: String,
    pub contract: Contract,
    pub epoch: u64,
    pub token: Option<String>,
    pub head: Option<CommitIndex>,
    pub indexed_order: u64,
    pub high_sequence: u64,
    pub checkpoints: BTreeMap<String, String>,
    pub activity_heads: super::super::activity_heads::ActivityHeads,
    pub pending: super::super::pending::PendingRanges,
}
impl Root {
    pub fn new(contract: Contract) -> Result<Self> {
        contract.validate()?;
        let store_id =
            digest(&serde_json::to_vec(&(&contract, crate::durable_fs::random_id()?)).map_err(|_| integrity())?);
        Ok(Self {
            store_id,
            contract,
            epoch: 0,
            token: None,
            head: None,
            indexed_order: 0,
            high_sequence: 0,
            checkpoints: BTreeMap::new(),
            activity_heads: Default::default(),
            pending: Default::default(),
        })
    }
    pub fn validate(&self, contract: &Contract) -> Result<()> {
        if &self.contract != contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        self.contract.validate()?;
        self.activity_heads.validate()?;
        self.pending.validate()?;
        if self.pending.latest_order() > self.head.as_ref().map_or(0, |head| head.order) {
            return Err(integrity());
        }
        if !valid_digest(&self.store_id)
            || (self.epoch == 0) != self.token.is_none()
            || self.token.as_ref().is_some_and(|token| !bounded_id(token))
        {
            return Err(integrity());
        }
        if let Some(head) = &self.head {
            head.validate(&self.store_id)?;
            if self.epoch == 0
                || self.high_sequence != head.high_sequence
                || self.indexed_order > head.order
                || head.order - self.indexed_order > 1
            {
                return Err(integrity());
            }
        } else if self.indexed_order != 0
            || self.high_sequence != 0
            || !self.checkpoints.is_empty()
            || !self.activity_heads.is_empty()
            || !self.pending.is_empty()
        {
            return Err(integrity());
        }
        if self.checkpoints.iter().any(|(key, value)| {
            !bounded_id(key) || value.is_empty() || value.len() > culprit_core::telemetry::MAX_CURSOR_BYTES
        }) {
            return Err(integrity());
        }
        Ok(())
    }
    pub fn require_owner(&self, fence: &Fence) -> Result<()> {
        if self.store_id != fence.store_id()
            || self.epoch != fence.epoch()
            || self.token.as_deref() != Some(fence.token())
        {
            return Err(DeliveryProblem::Fenced.error());
        }
        Ok(())
    }
    pub fn head_id(&self) -> Option<&str> {
        self.head.as_ref().map(|head| head.range_id.as_str())
    }
}

/// A traversal freezes its upper boundary. Concurrent commits appear in the next traversal.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryCursor {
    pub store_id: String,
    pub after: u64,
    pub head: CommitIndex,
    pub high_sequence: u64,
}
impl RecoveryCursor {
    pub fn validate(&self, root: &Root) -> Result<()> {
        self.head.validate(&root.store_id)?;
        if self.store_id != root.store_id
            || self.after >= self.head.order
            || self.high_sequence != self.head.high_sequence
            || self.high_sequence > root.high_sequence
            || root.head.as_ref().is_none_or(|head| head.order < self.head.order)
        {
            return Err(integrity());
        }
        Ok(())
    }
}
