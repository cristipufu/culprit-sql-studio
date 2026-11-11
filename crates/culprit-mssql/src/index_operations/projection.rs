//! Scan-only projections: aggregate exact native counters before converting to display numbers.
use MssqlIndexCounter::*;
use culprit_core::{
    IndexContention, Result,
    telemetry::{MssqlIndexCounter, MssqlIndexOperations},
};
use std::collections::BTreeMap;
type TableKey = (String, String);
type IndexKey = (String, String, String);
pub(super) struct Summary {
    pub forwarded: BTreeMap<TableKey, f64>,
    pub contention: BTreeMap<IndexKey, IndexContention>,
    pub forwarded_complete: bool,
    pub contention_complete: bool,
}
pub(super) fn summarize(rows: &[MssqlIndexOperations]) -> Result<Summary> {
    let mut heaps: BTreeMap<TableKey, Vec<&MssqlIndexOperations>> = BTreeMap::new();
    let mut indexes: BTreeMap<IndexKey, Vec<&MssqlIndexOperations>> = BTreeMap::new();
    let mut summary = Summary {
        forwarded: BTreeMap::new(),
        contention: BTreeMap::new(),
        forwarded_complete: true,
        contention_complete: true,
    };
    for row in rows {
        if row.engine_internal == Some(true) || row.object_type.as_deref().is_some_and(|kind| kind != "U") {
            continue;
        }
        let names = row
            .schema
            .as_ref()
            .zip(row.object.as_ref())
            .filter(|_| row.engine_internal == Some(false) && row.object_type.as_deref() == Some("U"));
        let Some((schema, object)) = names else {
            summary.forwarded_complete &= row.identity.index_id != 0;
            summary.contention_complete &= row.identity.index_id == 0;
            continue;
        };
        if row.identity.index_id == 0 {
            heaps.entry((schema.clone(), object.clone())).or_default().push(row);
        } else if let Some(index) = &row.index {
            indexes.entry((schema.clone(), object.clone(), index.clone())).or_default().push(row);
        } else {
            summary.contention_complete = false;
        }
    }
    for (key, rows) in heaps {
        if let Some(count) = sum(&rows, ForwardedFetches)? {
            summary.forwarded.insert(key, count as f64);
        } else {
            summary.forwarded_complete = false;
        }
    }
    for (key, rows) in indexes {
        match contention(&rows)? {
            Some(contention) => {
                // A measured zero is a baseline for a later scan, not missing evidence.
                summary.contention.insert(key, contention);
            }
            None => summary.contention_complete = false,
        }
    }
    Ok(summary)
}
fn sum(rows: &[&MssqlIndexOperations], kind: MssqlIndexCounter) -> Result<Option<u128>> {
    let mut sum = 0u128;
    for row in rows {
        let Some(value) = row.counter(kind) else { return Ok(None) };
        let value = value.parse::<u64>().map_err(|_| crate::read::invalid_data())?;
        sum = sum.checked_add(value as u128).ok_or_else(crate::read::invalid_data)?;
    }
    Ok(Some(sum))
}
fn contention(rows: &[&MssqlIndexOperations]) -> Result<Option<IndexContention>> {
    let mut counts = [0.0; 8];
    for (slot, kind) in counts.iter_mut().zip([
        RowLockWaits,
        RowLockWaitMs,
        PageLockWaits,
        PageLockWaitMs,
        PageLatchWaits,
        PageLatchWaitMs,
        LockEscalations,
        LockEscalationAttempts,
    ]) {
        let Some(count) = sum(rows, kind)? else { return Ok(None) };
        *slot = count as f64;
    }
    let [
        row_lock_waits,
        row_lock_wait_ms,
        page_lock_waits,
        page_lock_wait_ms,
        page_latch_waits,
        page_latch_wait_ms,
        lock_escalations,
        lock_escalation_attempts,
    ] = counts;
    let option = rows.first().and_then(|row| row.optimize_for_sequential_key);
    Ok(Some(IndexContention {
        reset: culprit_core::ResetMarker::Unknown,
        row_lock_waits,
        row_lock_wait_ms,
        page_lock_waits,
        page_lock_wait_ms,
        page_latch_waits,
        page_latch_wait_ms,
        lock_escalations,
        lock_escalation_attempts,
        leaf_inserts: sum(rows, LeafInserts)?.map(|count| count as f64),
        sequential_key: rows
            .iter()
            .all(|row| row.leading_key_is_identity == Some(true) || row.leading_key_default.is_some()),
        optimize_for_sequential_key: option
            .filter(|value| rows.iter().all(|row| row.optimize_for_sequential_key == Some(*value))),
    }))
}
