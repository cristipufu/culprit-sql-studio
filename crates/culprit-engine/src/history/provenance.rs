use std::path::Path;

use culprit_core::changes::epoch;
use culprit_core::{
    Capability, ChangeEvent, Engine, EventAction, EventKind, EventSource, IndexStat, MissingCapability, SizeBaseline,
    Snapshot,
};

use super::{MAX_BACKFILL, SAME_TIME, events_traced, is_usage_reset, same_database};
use crate::{reports, trends};

/// Date the indexes that may be new ([`IndexStat::created_at_latest`]) from this scan's changes (`current`) and the
/// earlier scans', so a check doesn't call an index unused before it has existed long enough to be read. Usage counters
/// the scans saw start over date [`IndexStat::counters_since`] and [`culprit_core::TableStat::counters_since`], and
/// count as a creation: what the counters say covers only the time since.
pub fn date_new_indexes(dir: &Path, snapshot: &mut Snapshot, current: &[ChangeEvent]) {
    let _ = date_new_indexes_traced(dir, snapshot, current);
}

pub(crate) fn date_new_indexes_traced(
    dir: &Path,
    snapshot: &mut Snapshot,
    current: &[ChangeEvent],
) -> Vec<culprit_core::BaselineReference> {
    let Some(now) = epoch(&snapshot.taken_at) else { return Vec::new() };
    let (mut known, mut inputs) = events_traced(dir, now - trends::HISTORY_DAYS * 86_400.0, now, Some(snapshot));
    known.extend(current.iter().cloned());
    let ids = reports::report_ids(dir);
    // A saved connection can have pointed elsewhere. Only a scan of this database can show that its indexes
    // already existed. Bound legacy report reads just as event backfill is bounded.
    let first = ids
        .iter()
        .filter(|id| reports::id_epoch(id).is_some_and(|t| t < now))
        .take(MAX_BACKFILL)
        .filter_map(|id| reports::load_snapshot(&dir.join(format!("{id}.json"))).ok())
        .find(|s| same_database(s, snapshot) && epoch(&s.taken_at).is_some_and(|t| t < now));
    let first_scan = first.as_ref().and_then(|s| epoch(&s.taken_at));
    if let Some(first) = first {
        let reference = culprit_core::ScanReference::of(&first);
        if !inputs.contains(&reference) {
            inputs.push(reference);
        }
    }
    // MariaDB's userstat counts from when it was turned on (or FLUSH INDEX_STATISTICS), which nothing dates: as far as
    // anyone knows, from the first of the scans that saw it counting.
    let userstat = usage_from_userstat(snapshot)
        .then(|| userstat_since(dir, snapshot, now, &mut inputs).unwrap_or(snapshot.taken_at.clone()));
    let later = |a: Option<String>, b: Option<&String>| match (a, b) {
        (Some(a), Some(b)) => Some(if epoch(&a).unwrap_or(0.0) >= epoch(b).unwrap_or(0.0) { a } else { b.clone() }),
        (a, b) => a.or_else(|| b.cloned()),
    };
    for i in &mut snapshot.indexes {
        // What the collector dated (a database restored after the server started), what the scans saw, and userstat's
        // start: the latest of them. A creation is no earlier than the counters watching it.
        let collected = i.counters_since.take();
        let counted =
            later(later(latest_usage_reset(&i.qualified_table(), &known), collected.as_ref()), userstat.as_ref());
        i.created_at_latest = later(latest_creation(i, &known, first_scan), counted.as_ref());
        i.counters_since = counted;
    }
    for t in &mut snapshot.tables {
        t.counters_since = latest_usage_reset(&t.qualified(), &known);
    }
    let mut references = Vec::new();
    for scan in inputs {
        let reference = culprit_core::BaselineReference {
            purpose: culprit_core::BaselinePurpose::IndexAge,
            scan,
            measurement: None,
        };
        if !references.contains(&reference) {
            references.push(reference);
        }
    }
    references
}

/// MariaDB counting index reads with userstat (performance_schema off).
fn usage_from_userstat(s: &Snapshot) -> bool {
    s.server.is_mariadb() && s.server.has(Capability::IndexUsage) && !s.server.has(Capability::PerformanceSchema)
}

/// Earlier scans read back to date userstat's counters.
const USERSTAT_SCANS: usize = 10;

/// When the newest unbroken run of scans counting with userstat began: the counters have counted since at least then.
/// `None` when no earlier scan did. A scan that counted otherwise, or not at all, ends the run: userstat was turned
/// off, and its counters started over when it was turned back on.
fn userstat_since(
    dir: &Path,
    current: &Snapshot,
    now: f64,
    inputs: &mut Vec<culprit_core::ScanReference>,
) -> Option<String> {
    let mut since = None;
    for id in reports::report_ids(dir)
        .iter()
        .rev()
        .filter(|id| reports::id_epoch(id).is_some_and(|t| t < now))
        .take(USERSTAT_SCANS)
    {
        let Ok(s) = reports::load_snapshot(&dir.join(format!("{id}.json"))) else { continue };
        if !same_database(&s, current) || epoch(&s.taken_at).is_none_or(|t| t >= now) {
            break;
        }
        inputs.push(culprit_core::ScanReference::of(&s));
        if !usage_from_userstat(&s) {
            break;
        }
        since = Some(s.taken_at);
    }
    since
}

/// The latest the usage counters of `table` (`schema.table`) started over, by the scans: for the table, or for the
/// whole database.
fn latest_usage_reset(table: &str, events: &[ChangeEvent]) -> Option<String> {
    events
        .iter()
        .filter(|e| is_usage_reset(e) && e.table.as_deref().is_none_or(|t| t.eq_ignore_ascii_case(table)))
        .max_by(|a, b| epoch(&a.at).unwrap_or(0.0).total_cmp(&epoch(&b.at).unwrap_or(0.0)))
        .map(|e| e.at.clone())
}

/// Hours a growth baseline must be behind the scan: under that, a size difference is noise turned into a rate.
const GROWTH_MIN_HOURS: f64 = culprit_core::PER_DAY_MIN_HOURS;
/// Scans read, newest first, looking for one old enough of the same database.
const GROWTH_CANDIDATES: usize = 5;

/// How big each table was at the newest earlier scan of the same database that is at least [`GROWTH_MIN_HOURS`] old.
/// The scan just before is often minutes old; growth measured over minutes and stretched to a day is no measurement.
pub fn growth_baseline(dir: &Path, current: &Snapshot) -> Option<SizeBaseline> {
    let cutoff = epoch(&current.taken_at)? - GROWTH_MIN_HOURS * 3600.0;
    reports::report_ids(dir)
        .iter()
        .rev()
        .filter(|id| reports::id_epoch(id).is_some_and(|t| t <= cutoff))
        .take(GROWTH_CANDIDATES)
        .filter_map(|id| reports::load_snapshot(&dir.join(format!("{id}.json"))).ok())
        .filter(|p| epoch(&p.taken_at).is_some_and(|t| t <= cutoff))
        .find(|p| same_database(p, current) && !p.tables.is_empty())
        .and_then(|p| SizeBaseline::from_scan(&p))
}

/// A replica's usage counters count the reads run on it (and, on SQL Server's secondaries, Query Store's history is the
/// primary's), so they say nothing about what the database's workload uses: index usage becomes missing, with the
/// reason, and the counters are set aside rather than read as zero. A standby doesn't count the writes it replays
/// either. Providers say they reached a replica with [`culprit_core::REPLICA_SETTING`].
pub fn set_aside_replica_usage(snapshot: &mut Snapshot) {
    let Some(role) = snapshot.setting(culprit_core::REPLICA_SETTING).map(|s| s.value.clone()) else { return };
    snapshot.server.capabilities.retain(|c| *c != Capability::IndexUsage);
    snapshot.server.missing.retain(|m| m.capability != Capability::IndexUsage);
    snapshot.server.missing.push(MissingCapability {
        capability: Capability::IndexUsage,
        reason: format!(
            "This connection reached a {role}: its usage counters count only the reads run on it, not the database's \
             whole workload, so no index is called unused from here."
        ),
        fix: Some(
            "Scan the primary to judge index use, and check every replica's reads before dropping an index.".into(),
        ),
    });
    for i in &mut snapshot.indexes {
        (i.reads, i.writes, i.seeks, i.scans, i.lookups, i.last_read) = (None, None, None, None, None, None);
    }
    let standby = snapshot.server.engine == Engine::Postgres;
    for t in &mut snapshot.tables {
        (t.seq_scans, t.seq_rows_read, t.index_scans) = (None, None, None);
        if standby {
            (t.inserts, t.updates, t.deletes, t.modifications_since_analyze) = (None, None, None, None);
        }
    }
}

/// The latest an index may have been created, when something says it may be new: a scan saw its table without it, or
/// saw the table appear, or the database dated a change to its table before Culprit's first scan (SQL Server and MySQL
/// move a table's date when one of its indexes is created). A table change dated after the first scan isn't one: the
/// scans would have seen a new index appear.
pub(super) fn latest_creation(i: &IndexStat, events: &[ChangeEvent], first_scan: Option<f64>) -> Option<String> {
    let table = i.qualified_table();
    events
        .iter()
        .filter(|e| {
            e.table.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(&table))
                || (is_usage_reset(e) && e.table.is_none())
        })
        .filter(|e| match (e.kind, e.action) {
            // Its usage counters started over: what they say covers only the time since, as for a new index.
            (EventKind::StatsReset, _) => is_usage_reset(e),
            (EventKind::Index, Some(EventAction::Created)) => {
                e.object.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&i.name))
            }
            (EventKind::Table, Some(EventAction::Created)) => true,
            (EventKind::Table, _) => {
                e.source == EventSource::Database && first_scan.is_none_or(|f| e.start() <= f + SAME_TIME)
            }
            _ => false,
        })
        .max_by(|a, b| epoch(&a.at).unwrap_or(0.0).total_cmp(&epoch(&b.at).unwrap_or(0.0)))
        .map(|e| e.at.clone())
}
