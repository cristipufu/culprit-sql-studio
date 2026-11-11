//! A small current-state index avoids replaying years of capture history to recover active incidents.
use super::{
    contract::{bounded_id, valid_digest},
    evidence::RangeManifest,
};
use culprit_core::{
    Result,
    telemetry::{CaptureRevision, DeliveryProblem, MAX_CURSOR_BYTES, MAX_FLEET_TARGETS},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActivityHead {
    pub scope_id: String,
    pub logical_source_id: String,
    pub endpoint_key: String,
    pub range_id: String,
    pub manifest_digest: String,
    pub revision: CaptureRevision,
}
impl ActivityHead {
    fn rank(&self) -> Result<(u64, u64)> {
        let positive = |value: &str| value.parse::<u64>().ok().filter(|value| *value > 0).ok_or_else(invalid);
        Ok((positive(&self.revision.owner_epoch)?, positive(&self.revision.sequence)?))
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.rank()?;
        if [&self.scope_id, &self.logical_source_id, &self.endpoint_key, &self.range_id, &self.manifest_digest]
            .iter()
            .any(|value| !valid_digest(value))
            || !bounded_id(&self.revision.outbox_id)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub struct ActivityHeadPage {
    pub heads: Vec<ActivityHead>,
    pub next_cursor: Option<String>,
    pub high_sequence: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ActivityHeads(BTreeMap<String, ActivityHead>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    snapshot: String,
    after: String,
}
impl ActivityHeads {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.0.len() > MAX_FLEET_TARGETS {
            return Err(DeliveryProblem::Capacity.error());
        }
        let mut endpoints = BTreeSet::new();
        for (scope, head) in &self.0 {
            head.validate()?;
            if scope != &head.logical_source_id || !endpoints.insert(&head.endpoint_key) {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub(crate) fn observe(&mut self, manifest: &RangeManifest, manifest_digest: &str) -> Result<()> {
        let Some(snapshot) = manifest.source.observation.activity() else {
            return Ok(());
        };
        if !snapshot.is_valid() || !snapshot.database_wide {
            return Err(invalid());
        }
        let source = &manifest.source;
        let scope_id = source.captured_range()?.scope_id;
        let logical_source_id = source.logical_source_id()?;
        let mut endpoint = source.clone();
        endpoint.database.incarnation = None;
        let endpoint_key = endpoint.logical_source_id()?;
        let candidate = ActivityHead {
            scope_id: scope_id.clone(),
            logical_source_id: logical_source_id.clone(),
            endpoint_key: endpoint_key.clone(),
            range_id: manifest.range_id.clone(),
            manifest_digest: manifest_digest.into(),
            revision: manifest.capture.revision.clone().ok_or_else(invalid)?,
        };
        candidate.validate()?;
        for previous in self
            .0
            .values()
            .filter(|head| head.logical_source_id == logical_source_id || head.endpoint_key == endpoint_key)
        {
            if previous.revision.outbox_id != candidate.revision.outbox_id {
                return Err(DeliveryProblem::Conflict.error());
            }
            if previous.rank()? > candidate.rank()? {
                return Ok(());
            }
            if previous.rank()? == candidate.rank()? && previous != &candidate {
                return Err(invalid());
            }
        }
        let replacing = self
            .0
            .values()
            .any(|head| head.logical_source_id == logical_source_id || head.endpoint_key == endpoint_key);
        if !replacing && self.0.len() == MAX_FLEET_TARGETS {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.0.retain(|_, head| head.logical_source_id != logical_source_id && head.endpoint_key != endpoint_key);
        self.0.insert(logical_source_id, candidate);
        Ok(())
    }
    pub(crate) fn page(&self, cursor: Option<&str>, limit: u32, high_sequence: u64) -> Result<ActivityHeadPage> {
        self.validate()?;
        if limit == 0 || limit > 128 {
            return Err(DeliveryProblem::Capacity.error());
        }
        let snapshot = culprit_core::telemetry::digest(&serde_json::to_vec(self).map_err(|_| invalid())?);
        let after = match cursor {
            Some(text) if text.len() <= MAX_CURSOR_BYTES => {
                let cursor: Cursor = serde_json::from_str(text).map_err(|_| invalid())?;
                if cursor.snapshot != snapshot {
                    return Err(DeliveryProblem::Conflict.error());
                }
                if !valid_digest(&cursor.after) {
                    return Err(invalid());
                }
                cursor.after
            }
            Some(_) => return Err(invalid()),
            None => String::new(),
        };
        let mut heads = self
            .0
            .range((Bound::Excluded(after), Bound::Unbounded))
            .take(limit as usize + 1)
            .map(|(_, head)| head.clone())
            .collect::<Vec<_>>();
        let next_cursor = if heads.len() > limit as usize {
            heads.pop();
            Some(
                serde_json::to_string(&Cursor {
                    snapshot,
                    after: heads.last().ok_or_else(invalid)?.logical_source_id.clone(),
                })
                .map_err(|_| invalid())?,
            )
        } else {
            None
        };
        Ok(ActivityHeadPage { heads, next_cursor, high_sequence })
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::batch_tests::quiet_activity;
    fn manifest() -> RangeManifest {
        let root = tempfile::tempdir().unwrap();
        quiet_activity(root.path()).manifest().clone()
    }
    fn revision(manifest: &mut RangeManifest, epoch: u64, sequence: u64) {
        let revision = manifest.capture.revision.as_mut().unwrap();
        revision.owner_epoch = epoch.to_string();
        revision.sequence = sequence.to_string();
        manifest.range_id =
            culprit_core::telemetry::digest(&serde_json::to_vec(&(&manifest.contract, &manifest.capture)).unwrap());
    }
    #[test]
    fn revision_order_survives_late_commits_and_complete_empty_activity_is_a_real_head() {
        let mut heads = ActivityHeads::default();
        let mut current = manifest();
        revision(&mut current, 2, 2);
        heads.observe(&current, &current.digest().unwrap()).unwrap();
        let expected = heads.page(None, 10, 2).unwrap().heads;
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].range_id, current.range_id);
        let mut older = current.clone();
        revision(&mut older, 1, 999);
        heads.observe(&older, &older.digest().unwrap()).unwrap();
        assert_eq!(heads.page(None, 10, 999).unwrap().heads, expected);
        let mut conflict = current.clone();
        conflict.collected_until = "2020-01-01T06:00:00Z".into();
        assert!(heads.observe(&conflict, &conflict.digest().unwrap()).is_err());
    }
    #[test]
    fn paging_is_frozen_and_new_incarnations_replace_only_the_older_endpoint_head() {
        let mut heads = ActivityHeads::default();
        let first = manifest();
        heads.observe(&first, &first.digest().unwrap()).unwrap();
        let mut second = first.clone();
        second.source.database.database = "other".into();
        revision(&mut second, 1, 2);
        heads.observe(&second, &second.digest().unwrap()).unwrap();
        let page = heads.page(None, 1, 2).unwrap();
        assert_eq!(page.heads.len(), 1);
        assert!(page.next_cursor.is_some());
        let last = heads.page(page.next_cursor.as_deref(), 1, 2).unwrap();
        assert_eq!(last.heads.len(), 1);
        assert!(last.next_cursor.is_none());
        let mut replacement = first.clone();
        replacement.source.database.incarnation = Some("new-native-database".into());
        revision(&mut replacement, 1, 3);
        heads.observe(&replacement, &replacement.digest().unwrap()).unwrap();
        assert_eq!(heads.page(None, 10, 3).unwrap().heads.len(), 2);
        assert!(heads.page(page.next_cursor.as_deref(), 1, 3).is_err());
        assert!(heads.page(None, 0, 3).is_err());
    }
    #[test]
    fn a_new_native_epoch_replaces_the_logical_head_while_preserving_its_distinct_captured_scope() {
        let mut heads = ActivityHeads::default();
        let first = manifest();
        heads.observe(&first, &first.digest().unwrap()).unwrap();
        let previous = heads.page(None, 10, 1).unwrap().heads.remove(0);
        let mut restarted = first.clone();
        revision(&mut restarted, 1, 2);
        let culprit_core::telemetry::SourceObservation::Activity(snapshot) = &mut restarted.source.observation else {
            unreachable!()
        };
        snapshot.epoch = "restarted-native-epoch".into();
        heads.observe(&restarted, &restarted.digest().unwrap()).unwrap();
        let current = heads.page(None, 10, 2).unwrap().heads;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].logical_source_id, previous.logical_source_id);
        assert_eq!(current[0].endpoint_key, previous.endpoint_key);
        assert_ne!(current[0].scope_id, previous.scope_id);
        assert_eq!(current[0].range_id, restarted.range_id);
    }
}
