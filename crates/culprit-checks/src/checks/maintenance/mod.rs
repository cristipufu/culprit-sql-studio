//! Maintenance checks: statistics gone stale, heaps, dead rows autovacuum hasn't reached, and transaction ID age.
//! What they share (autovacuum's thresholds for a table) is here; each check is a file of its own.

use culprit_core::{
    Capability, Category, CheckInfo, Confidence, Engine, Finding, Fix, Metric, Severity, Snapshot, SubjectKind, Unit,
    format,
};

use culprit_core::{Check, t};

use crate::support::*;

mod dead_rows;
mod heap_table;
mod multixact_age;
mod stale_stats;
#[cfg(test)]
mod words;
mod xid_age;

pub use dead_rows::*;
pub use heap_table::*;
pub use stale_stats::*;
pub use xid_age::*;

/// The checks this area registers (`crate::all()` runs every area's), in the order their findings tie.
pub(crate) const CHECKS: &[&dyn Check] =
    &[&StaleStatsCheck, &HeapTableCheck, &DeadRowsCheck, &XidAgeCheck, &multixact_age::MultixactAgeCheck];

fn short_date(iso: &str) -> String {
    iso.get(..10).unwrap_or(iso).to_string()
}

/// What holds cleanup back, and how far behind when known: "session 4242, 5.0M transactions behind".
pub(super) fn cleanup_holder_phrase(h: &culprit_core::OpenTransaction) -> String {
    use culprit_core::CleanupHolder::{PreparedTransaction, ReplicationSlot, Session};
    let name = &h.session;
    match (h.holder, h.xid_age.filter(|a| *a > 0.0).map(format::count)) {
        (PreparedTransaction, None) => t!("common.cleanup_holder.prepared", name = name),
        (PreparedTransaction, Some(behind)) => {
            t!("common.cleanup_holder.prepared_behind", name = name, behind = behind)
        }
        (ReplicationSlot, None) => t!("common.cleanup_holder.slot", name = name),
        (ReplicationSlot, Some(behind)) => t!("common.cleanup_holder.slot_behind", name = name, behind = behind),
        (Session, None) => t!("common.cleanup_holder.session", name = name),
        (Session, Some(behind)) => t!("common.cleanup_holder.session_behind", name = name, behind = behind),
    }
}

/// Dead rows past this many times a table's autovacuum trigger mean autovacuum is behind on it, not about to run.
const DEAD_MARGIN: f64 = 2.0;

/// The rows changed (`analyze`) or dead (`vacuum`) at which autovacuum visits `t`: the table's own setting, else the
/// server's, else PostgreSQL's default. `None` with autovacuum off for it.
fn autovacuum_trigger(snapshot: &Snapshot, t: &culprit_core::TableStat, live: f64, kind: &str) -> Option<f64> {
    let off = t.storage.as_ref().is_some_and(|s| s.autovacuum_off())
        || snapshot.setting("autovacuum").is_some_and(|s| !matches!(s.value.trim(), "on" | "true" | "1"));
    if off {
        return None;
    }
    let threshold = autovacuum_setting(snapshot, t, &format!("autovacuum_{kind}_threshold")).unwrap_or(50.0);
    let factor = autovacuum_setting(snapshot, t, &format!("autovacuum_{kind}_scale_factor"))
        .unwrap_or(if kind == "analyze" { 0.1 } else { 0.2 });
    let trigger = threshold + factor * live;
    let cap = (kind == "vacuum").then(|| autovacuum_setting(snapshot, t, "autovacuum_vacuum_max_threshold")).flatten();
    Some(cap.filter(|c| *c >= 0.0).map_or(trigger, |c| trigger.min(c)))
}
