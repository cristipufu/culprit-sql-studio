//! What changed between scans. Each scan compares itself with the scan before it (tables, columns, indexes, settings,
//! version, restarts, a workload statistics reset, optimizer statistics) and keeps the result next to its report as
//! `<snapshot id>.events.json`, together with the changes the database dated itself. Reading a timeline is then a
//! matter of reading small files; a report whose file couldn't be written is compared the first time it is read.

mod diff;
mod provenance;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use culprit_core::changes::epoch;
use culprit_core::events::in_range;
use culprit_core::{ChangeEvent, EventKind, Snapshot};

use crate::{private_fs, reports};

pub use self::diff::diff;
pub(crate) use self::provenance::date_new_indexes_traced;
pub use self::provenance::{date_new_indexes, growth_baseline, set_aside_replica_usage};

pub const EVENTS_SUFFIX: &str = ".events.json";
/// Reports compared on first read per call; the rest wait for the next read.
pub(super) const MAX_BACKFILL: usize = 30;
/// Clocks drift by a second or two between readings of the same start time.
pub(super) const SAME_TIME: f64 = 120.0;
/// Subject of the events that say usage counters started over.
pub(super) const USAGE: &str = "index usage";

pub(super) fn is_usage_reset(e: &ChangeEvent) -> bool {
    e.kind == EventKind::StatsReset && e.object.as_deref() == Some(USAGE)
}

/// Whether two scans are of the same database, so their counters and sizes can be compared: the same engine and
/// database (a saved connection can be pointed at another).
pub fn same_database(p: &Snapshot, c: &Snapshot) -> bool {
    p.same_database(c)
}

fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{EVENTS_SUFFIX}"))
}

#[derive(Serialize, Deserialize)]
pub(super) struct EventRecord {
    pub(super) inputs: Vec<culprit_core::ScanReference>,
    pub(super) events: Vec<ChangeEvent>,
}

pub(super) fn read(dir: &Path, id: &str) -> Option<EventRecord> {
    let mut record: EventRecord = std::fs::read(path(dir, id)).ok().and_then(|b| serde_json::from_slice(&b).ok())?;
    for event in &mut record.events {
        event.refresh_language();
    }
    Some(record)
}

fn event_inputs(current: &Snapshot, previous: Option<&Snapshot>) -> Vec<culprit_core::ScanReference> {
    let mut inputs = vec![culprit_core::ScanReference::of(current)];
    if let Some(previous) = previous.filter(|p| {
        same_database(p, current) && epoch(&p.taken_at).zip(epoch(&current.taken_at)).is_some_and(|(a, b)| a < b)
    }) {
        inputs.push(culprit_core::ScanReference::of(previous));
    }
    inputs
}

/// Keep a scan's changes ([`diff`]) next to its report. Never fails a scan: problems leave no file, and the scan is
/// compared again the first time it is read.
pub(crate) fn keep_scan(dir: &Path, current: &Snapshot, previous: Option<&Snapshot>, events: &[ChangeEvent]) {
    let mut events = events.to_vec();
    for event in &mut events {
        event.clear_rendered_text();
    }
    let record = EventRecord { inputs: event_inputs(current, previous), events };
    if let Ok(json) = serde_json::to_vec(&record) {
        let _ = private_fs::write_atomic(&path(dir, &current.id), &json);
    }
}

/// Everything Culprit's scans noticed in `[start, end]` (epoch seconds), oldest first.
pub fn events(dir: &Path, start: f64, end: f64) -> Vec<ChangeEvent> {
    events_traced(dir, start, end, None).0
}

pub(super) fn events_traced(
    dir: &Path,
    start: f64,
    end: f64,
    current: Option<&Snapshot>,
) -> (Vec<ChangeEvent>, Vec<culprit_core::ScanReference>) {
    let ids = reports::report_ids(dir);
    let mut out = Vec::new();
    let mut inputs = Vec::new();
    let mut backfilled = 0;
    let mut previous: Option<Snapshot> = None;
    for (i, id) in ids.iter().enumerate() {
        // A scan's file holds what happened since the scan before it (or, for the first, what the database dated).
        let wanted = reports::id_epoch(id).is_some_and(|t| t >= start - 1.0 && (current.is_none() || t <= end));
        if let Some(record) = read(dir, id) {
            if wanted
                && current.is_none_or(|s| {
                    !record.inputs.is_empty() && record.inputs.iter().all(|r| reference_matches(r, s, end))
                })
            {
                out.extend(record.events);
                inputs.extend(record.inputs);
            }
            previous = None;
            continue;
        }
        if !wanted || backfilled >= MAX_BACKFILL {
            previous = None;
            continue;
        }
        backfilled += 1;
        let Ok((scan, recorded)) = reports::load_snapshot_and_recorded_changes(&dir.join(format!("{id}.json"))) else {
            previous = None;
            continue;
        };
        let before = previous.take().or_else(|| {
            let prior = ids.get(i.checked_sub(1)?)?;
            reports::load_snapshot(&dir.join(format!("{prior}.json"))).ok()
        });
        let events = diff(before.as_ref(), &scan, recorded);
        let references = event_inputs(&scan, before.as_ref());
        if current.is_none_or(|s| references.iter().all(|r| reference_matches(r, s, end))) {
            inputs.extend(references);
            out.extend(events.iter().cloned());
        }
        keep_scan(dir, &scan, before.as_ref(), &events);
        previous = Some(scan);
    }
    if current.is_some() {
        // An interval ending after this scan cannot date an index yet, even if its beginning is inside the window.
        out.retain(|e| e.end() <= end);
    }
    (in_range(out, start, end), inputs)
}

fn reference_matches(reference: &culprit_core::ScanReference, current: &Snapshot, end: f64) -> bool {
    epoch(&reference.taken_at).is_some_and(|t| t <= end)
        && reference.database.as_ref().is_some_and(|db| db.comparison(&current.database_identity()).allows_comparison())
}
