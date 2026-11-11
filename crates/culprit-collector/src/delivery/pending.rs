//! Bounded recovery work index. Historical native commits remain retained after analytical publication.
use super::{contract::valid_digest, recovery::RecoveryOrder};
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Included};

const MAX_PENDING_RANGES: usize = 20_000;

#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct PendingRanges(BTreeMap<u64, String>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    store_id: String,
    upper: u64,
    after: u64,
    order: RecoveryOrder,
}

impl PendingRanges {
    pub fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        if self.0.len() > MAX_PENDING_RANGES
            || self.0.iter().any(|(order, id)| *order == 0 || !valid_digest(id) || !ids.insert(id))
        {
            return Err(DeliveryProblem::Integrity.error());
        }
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn latest_order(&self) -> u64 {
        self.0.last_key_value().map_or(0, |(order, _)| *order)
    }
    pub fn insert(&mut self, order: u64, id: &str) -> Result<()> {
        if !valid_digest(id) || order == 0 {
            return Err(DeliveryProblem::Integrity.error());
        }
        if let Some(existing) = self.0.get(&order) {
            return if existing == id { Ok(()) } else { Err(DeliveryProblem::Integrity.error()) };
        }
        if self.0.values().any(|existing| existing == id) {
            return Err(DeliveryProblem::Integrity.error());
        }
        if self.0.len() >= MAX_PENDING_RANGES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.0.insert(order, id.into());
        Ok(())
    }
    pub fn retire(&mut self, id: &str) {
        self.0.retain(|_, existing| existing != id);
    }
    pub fn page(
        &self,
        cursor: Option<&str>,
        limit: u32,
        store_id: &str,
        committed_order: u64,
        order: RecoveryOrder,
    ) -> Result<(Vec<String>, Option<String>)> {
        self.validate()?;
        if !(1..=128).contains(&limit) {
            return Err(DeliveryProblem::Capacity.error());
        }
        if !valid_digest(store_id) || self.latest_order() > committed_order {
            return Err(DeliveryProblem::Integrity.error());
        }
        let cursor = match cursor {
            Some(text) if text.len() <= 512 => {
                let cursor: Cursor = serde_json::from_str(text).map_err(|_| DeliveryProblem::Integrity.error())?;
                if cursor.store_id != store_id
                    || cursor.order != order
                    || cursor.upper > committed_order
                    || cursor.after > cursor.upper
                    || (order == RecoveryOrder::OldestFirst && cursor.after == cursor.upper)
                {
                    return Err(DeliveryProblem::Integrity.error());
                }
                cursor
            }
            Some(_) => return Err(DeliveryProblem::Capacity.error()),
            None => Cursor { store_id: store_id.into(), upper: committed_order, after: 0, order },
        };
        let mut ids: Box<dyn Iterator<Item = (&u64, &String)>> = match order {
            RecoveryOrder::OldestFirst => Box::new(self.0.range((Excluded(cursor.after), Included(cursor.upper)))),
            RecoveryOrder::NewestFirst => {
                let upper = if cursor.after == 0 { Included(cursor.upper) } else { Excluded(cursor.after) };
                Box::new(self.0.range((Excluded(0), upper)).rev())
            }
        };
        let mut last = cursor.after;
        let page = ids
            .by_ref()
            .take(limit as usize)
            .map(|(order, id)| {
                last = *order;
                id.clone()
            })
            .collect::<Vec<_>>();
        let next = if ids.next().is_some() {
            Some(
                serde_json::to_string(&Cursor { after: last, ..cursor })
                    .map_err(|_| DeliveryProblem::Integrity.error())?,
            )
        } else {
            None
        };
        Ok((page, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::digest;
    #[test]
    fn newest_first_pages_are_frozen_and_remain_valid_after_retirement() {
        let store = digest(b"store");
        let mut pending = PendingRanges::default();
        let ids = (1..=4).map(|n| digest(n.to_string().as_bytes())).collect::<Vec<_>>();
        for (index, id) in ids[..3].iter().enumerate() {
            pending.insert(index as u64 + 1, id).unwrap();
        }
        let (first, cursor) = pending.page(None, 1, &store, 3, RecoveryOrder::NewestFirst).unwrap();
        assert_eq!(first, [ids[2].clone()]);
        pending.retire(&ids[2]);
        pending.insert(4, &ids[3]).unwrap();
        let (second, cursor) = pending.page(cursor.as_deref(), 1, &store, 4, RecoveryOrder::NewestFirst).unwrap();
        assert_eq!(second, [ids[1].clone()]);
        assert!(pending.page(cursor.as_deref(), 1, &store, 4, RecoveryOrder::OldestFirst).is_err());
        pending.retire(&ids[1]);
        let (last, end) = pending.page(cursor.as_deref(), 1, &store, 4, RecoveryOrder::NewestFirst).unwrap();
        assert_eq!(last, [ids[0].clone()]);
        assert!(end.is_none());
        assert_eq!(pending.page(None, 1, &store, 4, RecoveryOrder::NewestFirst).unwrap().0, [ids[3].clone()]);
    }

    #[test]
    fn retirement_during_recovery_keeps_the_cursor_valid_and_new_commits_wait_for_the_next_traversal() {
        let store = digest(b"store");
        let ids = [digest(b"one"), digest(b"two"), digest(b"three")];
        let mut pending = PendingRanges::default();
        assert_eq!(pending.page(None, 1, &store, 0, RecoveryOrder::OldestFirst).unwrap(), (vec![], None));
        pending.insert(1, &ids[0]).unwrap();
        pending.insert(2, &ids[1]).unwrap();
        let (first, cursor) = pending.page(None, 1, &store, 2, RecoveryOrder::OldestFirst).unwrap();
        assert_eq!(first, [ids[0].clone()]);
        pending.retire(&ids[0]);
        pending.insert(3, &ids[2]).unwrap();
        let (second, next) = pending.page(cursor.as_deref(), 1, &store, 3, RecoveryOrder::OldestFirst).unwrap();
        assert_eq!(second, [ids[1].clone()]);
        assert!(next.is_none(), "a later append cannot extend a frozen recovery traversal");
        assert!(
            pending.page(cursor.as_deref(), 1, &digest(b"different store"), 3, RecoveryOrder::OldestFirst).is_err()
        );
        let (_, cursor) = pending.page(None, 1, &store, 3, RecoveryOrder::OldestFirst).unwrap();
        pending.retire(&ids[1]);
        assert_eq!(
            pending.page(cursor.as_deref(), 1, &store, 3, RecoveryOrder::OldestFirst).unwrap(),
            (vec![ids[2].clone()], None)
        );
    }
}
