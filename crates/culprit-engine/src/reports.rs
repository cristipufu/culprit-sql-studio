//! Reports on disk, one JSON file per scan: `<data>/reports/<connection>/<snapshot id>.json`, plus
//! `<snapshot id>.counters.json` with the running totals that trends for cumulative sources are built from.

use std::fs;
use std::path::{Path, PathBuf};

use culprit_core::{Error, MINUTES_PER_DAY, Report, Result, window_label};

use crate::private_fs;
use crate::trends::{self, Counters};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

mod storage;

/// The prepared report for clients. Retained replay/collection copies stay in the report store.
pub fn display(report: &Report) -> impl Serialize + '_ {
    #[derive(Serialize)]
    struct View<'a> {
        snapshot: &'a culprit_core::Snapshot,
        findings: &'a [culprit_core::Finding],
        locked: &'a [culprit_core::LockedCheck],
        summary: &'a culprit_core::Summary,
        assessments: &'a [culprit_core::CheckAssessment],
        #[serde(skip_serializing_if = "<[culprit_core::RefinementDecision]>::is_empty")]
        refinement: &'a [culprit_core::RefinementDecision],
        #[serde(skip_serializing_if = "Option::is_none")]
        provenance: &'a Option<culprit_core::AnalysisProvenance>,
        #[serde(skip_serializing_if = "Option::is_none")]
        memory_failures: &'a Option<culprit_core::RetainedFailureReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        native_failures: &'a Option<culprit_core::RetainedFailureReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_memory: &'a Option<culprit_core::MemoryParticipantsReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_memory: &'a Option<culprit_core::MemoryParticipantsReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        page_waits: &'a Option<culprit_core::PageWaitReport>,
    }
    let Report {
        snapshot,
        findings,
        locked,
        summary,
        assessments,
        refinement,
        provenance,
        memory_failures,
        native_failures,
        request_memory,
        session_memory,
        page_waits,
        evidence: _,
    } = report;
    View {
        snapshot,
        findings,
        locked,
        summary,
        assessments,
        refinement,
        provenance,
        memory_failures,
        native_failures,
        request_memory,
        session_memory,
        page_waits,
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ReportSummary {
    /// The format of the report it points to ([`culprit_core::SNAPSHOT_FORMAT`]). A summary without it, or with
    /// another, points to a report another version saved, which this one doesn't read: it is passed over as if absent.
    pub format: u32,
    pub id: String,
    pub taken_at: String,
    pub opportunities: u32,
    pub headline: String,
    /// The scan's window in minutes; `None` for a scan taken without options.
    pub window_minutes: Option<u32>,
    /// The server as the scan saw it ("MariaDB 11.8.9"), so a list of connections can tell MariaDB from MySQL.
    pub product: Option<String>,
    /// Why the scan has no query history ("Query Store is OFF on this database."); `None` when it has one.
    pub no_workload: Option<String>,
    #[serde(default)]
    pub takeaway: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub headline_message: Option<culprit_core::i18n::Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub takeaway_message: Option<culprit_core::i18n::Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub no_workload_message: Option<culprit_core::i18n::Message>,
}

impl ReportSummary {
    fn render_messages(&mut self) {
        if let Some(message) = &self.no_workload_message {
            self.no_workload = Some(message.render());
        }
        if let Some(message) = &self.headline_message {
            self.headline = message.render();
        }
        if let Some(message) = &self.takeaway_message {
            self.takeaway = Some(message.render());
        }
    }

    fn for_storage(&self) -> Self {
        let mut stored = self.clone();
        if stored.no_workload_message.is_some() {
            stored.no_workload = None;
        }
        if stored.headline_message.is_some() {
            stored.headline.clear();
        }
        if stored.takeaway_message.is_some() {
            stored.takeaway = None;
        }
        stored
    }
}

/// The pointer to the latest scan with a window (`latest-7d.json`, `latest-6h.json`), or to the latest of any.
fn summary_path(dir: &Path, window_minutes: Option<u32>) -> PathBuf {
    match window_minutes {
        Some(minutes) => dir.join(format!("latest-{}.json", window_label(minutes))),
        None => dir.join("latest.json"),
    }
}

fn summary_of(report: &Report) -> ReportSummary {
    ReportSummary {
        format: report.snapshot.format,
        id: report.snapshot.id.clone(),
        taken_at: report.snapshot.taken_at.clone(),
        opportunities: report.summary.opportunities,
        headline: report.summary.headline.clone(),
        window_minutes: report.snapshot.options.as_ref().map(|o| o.window_in_minutes()),
        product: Some(report.snapshot.server.product.clone()),
        no_workload: report.snapshot.no_workload_reason(),
        takeaway: report.summary.takeaway.clone(),
        headline_message: report.summary.headline_message.clone(),
        takeaway_message: report.summary.takeaway_message.clone(),
        no_workload_message: report.snapshot.no_workload_message(),
    }
}

pub fn save(dir: &Path, report: &Report) -> Result<()> {
    private_fs::create_dir(dir).map_err(|e| Error::internal(format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{}.json", report.snapshot.id));
    let json = storage::encode(report).map_err(|e| Error::internal(e.to_string()))?;
    let _lock = private_fs::lock(&dir.join("latest.json")).map_err(|e| Error::internal(e.to_string()))?;
    write_atomic(&path, &json)?;
    if let Some(counters) = Counters::of(report) {
        let json = serde_json::to_vec(&counters).map_err(|e| Error::internal(e.to_string()))?;
        write_atomic(&counters_path(dir, &report.snapshot.id), &json)?;
    }
    let summary = summary_of(report);
    let json = serde_json::to_vec_pretty(&summary.for_storage()).map_err(|e| Error::internal(e.to_string()))?;
    write_atomic(&dir.join(format!("{}.summary.json", report.snapshot.id)), &json)?;
    if latest_summary(dir, None).is_none_or(|old| may_replace_latest(&old, &summary)) {
        write_atomic(&summary_path(dir, None), &json)?;
    }
    if let Some(minutes) = summary.window_minutes
        && latest_summary(dir, Some(minutes)).is_none_or(|old| may_replace_latest(&old, &summary))
    {
        write_atomic(&summary_path(dir, Some(minutes)), &json)?;
    }
    Ok(())
}

fn may_replace_latest(old: &ReportSummary, new: &ReportSummary) -> bool {
    fn time(summary: &ReportSummary) -> Option<chrono::DateTime<chrono::Utc>> {
        let at = chrono::DateTime::parse_from_rfc3339(&summary.taken_at).ok()?.to_utc();
        // Workspace timestamps have second precision, but their IDs distinguish scans within that second.
        let precise = chrono::NaiveDateTime::parse_from_str(&summary.id, "%Y%m%dT%H%M%S%.fZ")
            .ok()
            .map(|t| t.and_utc())
            .filter(|id| at.timestamp_subsec_nanos() == 0 && id.timestamp() == at.timestamp());
        Some(precise.unwrap_or(at))
    }
    // A timestamp that doesn't parse can't establish an ordering: the last writer wins.
    time(old).zip(time(new)).is_none_or(|(old, new)| old <= new)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    private_fs::write_atomic(path, bytes).map_err(|e| Error::internal(format!("{}: {e}", path.display())))
}

const COUNTERS_SUFFIX: &str = ".counters.json";

fn counters_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{COUNTERS_SUFFIX}"))
}

/// `<snapshot id>.json`: ids are UTC timestamps, so anything else in the directory (latest pointers, sidecars, marks,
/// the cloud cache) is not a report.
fn is_report_file(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.ends_with(".json")
        && name.starts_with(|c: char| c.is_ascii_digit())
        && !name.ends_with(COUNTERS_SUFFIX)
        && !name.ends_with(crate::history::EVENTS_SUFFIX)
        && !name.ends_with(".summary.json")
}

/// Snapshot ids of every saved report, oldest first.
pub(crate) fn report_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_report_file(p))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    ids.sort();
    ids
}

/// Running totals of every saved scan in the last [`trends::HISTORY_DAYS`] days, oldest first, from their counters files.
pub fn counters_history(dir: &Path, now: &str) -> Vec<Counters> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let oldest = trends::epoch(now).map(|t| t - trends::HISTORY_DAYS * 86_400.0).unwrap_or(f64::MIN);
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths.iter().filter(|p| is_report_file(p)) {
        let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        // Snapshot ids are their UTC timestamps (`20261011T120000.000Z`); skip old ones without opening them.
        if id_epoch(&id).is_some_and(|t| t < oldest) {
            continue;
        }
        if let Some(c) =
            fs::read(counters_path(dir, &id)).ok().and_then(|b| serde_json::from_slice::<Counters>(&b).ok())
        {
            out.push(c);
        }
    }
    out.retain(|c| trends::epoch(&c.taken_at).is_some_and(|t| t >= oldest));
    out.sort_by(|a, b| a.taken_at.cmp(&b.taken_at));
    out
}

/// A saved scan read again with what the scans saved beside it in `dir` say: pg_stat_statements' evictions against the
/// oldest scan a day or more back ([`evictions_baseline`]), the baseline a scan taken now would use. A retained
/// scan-time baseline remains evidence when its original report was pruned; absence of that file does not erase
/// the measurements stored in this scan. Writes nothing.
pub fn rebase_on_scans(dir: &Path, snapshot: &mut culprit_core::Snapshot) {
    if let Some(baseline) = evictions_baseline(dir, snapshot) {
        snapshot.evictions_before = Some(baseline);
    }
    apply_counter_baselines(dir, snapshot, None);
    rebuild_timeline(dir, snapshot);
}

/// Compare lock escalations, index lock contention and operational counters against saved scans in `dir`.
/// For any selected workload window (1h, 6h, 24h, 7d, 30d or custom), walks backward through compatible scans of the
/// same continuous server uptime inside that window so a scan taken a minute or a few hours ago does not hide
/// escalations or waits that happened earlier in the window. Writes nothing.
pub(crate) fn apply_counter_baselines(
    dir: &Path,
    snapshot: &mut culprit_core::Snapshot,
    immediate: Option<&culprit_core::Snapshot>,
) {
    if !snapshot.indexes.iter().any(|i| i.contention.is_some())
        && snapshot.operations.counters.is_empty()
        && snapshot.operations.sqlite.is_none()
    {
        return;
    }
    let now = trends::epoch(&snapshot.taken_at);
    let window_secs = f64::from(snapshot.options.as_ref().map_or(MINUTES_PER_DAY, |o| o.window_in_minutes())) * 60.0;
    let min_cover_secs = window_secs * 0.8;
    let max_age_secs = window_secs * 1.25;
    let lookback_secs = (trends::HISTORY_DAYS * 86_400.0).max(max_age_secs);

    let mut candidates: Vec<culprit_core::Snapshot> = Vec::new();
    if let Some(prev) = immediate.filter(|p| {
        p.id != snapshot.id
            && crate::history::same_database(p, snapshot)
            && trends::epoch(&p.taken_at).zip(now).is_some_and(|(at, now)| at <= now)
    }) {
        candidates.push(prev.clone());
    }
    for id in report_ids(dir)
        .into_iter()
        .rev()
        .filter(|id| {
            *id != snapshot.id && id_epoch(id).zip(now).is_some_and(|(at, now)| at < now && at >= now - lookback_secs)
        })
        .take(200)
    {
        if candidates.iter().any(|c| c.id == id) {
            continue;
        }
        let Ok(previous) = load_snapshot(&dir.join(format!("{id}.json"))) else { continue };
        if !crate::history::same_database(&previous, snapshot)
            || trends::epoch(&previous.taken_at).zip(now).is_none_or(|(at, now)| at >= now)
        {
            continue;
        }
        candidates.push(previous);
    }

    let mut chosen: Option<&culprit_core::Snapshot> = None;
    let mut zero_escalation_cover: Option<&culprit_core::Snapshot> = None;
    let mut zero_contention_cover: Option<&culprit_core::Snapshot> = None;
    let current_escalations = culprit_core::escalations_by_table(snapshot);
    for candidate in &candidates {
        let has_baseline = culprit_core::EscalationBaseline::from_scan(candidate, snapshot).is_some()
            || culprit_core::ContentionBaseline::from_scan(candidate, snapshot).is_some();
        if chosen.is_none() {
            // A reset or unknown start in the closest matching scan cannot be bypassed by an older counter.
            if !has_baseline && candidate.server.started_at.is_none() {
                snapshot.escalations_before = None;
                snapshot.contention_before = None;
                return;
            }
            chosen = Some(candidate);
            if culprit_core::EscalationBaseline::from_scan(candidate, snapshot).is_some_and(|b| b.restarted) {
                break;
            }
            continue;
        }
        let same_uptime = culprit_core::EscalationBaseline::from_scan(candidate, snapshot)
            .is_some_and(|b| !b.restarted)
            || culprit_core::ContentionBaseline::from_scan(candidate, snapshot).is_some();
        if !same_uptime {
            break;
        }
        let age_secs = trends::epoch(&candidate.taken_at).zip(now).map(|(at, now)| now - at);
        if age_secs.is_some_and(|age| age <= max_age_secs) {
            chosen = Some(candidate);
            continue;
        }
        // When the oldest scan inside the window covers only a fraction of it (for example, a scan 1 minute ago before
        // selecting 1h or 6h), an older scan from the same uptime with identical counter totals proves no escalations
        // or lock waits happened across the entire selected window.
        let chosen_age =
            chosen.and_then(|c| trends::epoch(&c.taken_at).zip(now)).map(|(at, now)| now - at).unwrap_or(0.0);
        if chosen_age < min_cover_secs {
            if culprit_core::escalations_by_table(candidate) == current_escalations {
                zero_escalation_cover = Some(candidate);
            }
            if culprit_core::ContentionBaseline::from_scan(candidate, snapshot).is_some_and(|b| {
                snapshot
                    .indexes
                    .iter()
                    .filter(|i| i.contention.is_some())
                    .all(|i| b.since(i).is_some_and(|d| d.waits == 0.0 && d.wait_ms == 0.0))
            }) {
                zero_contention_cover = Some(candidate);
            }
        }
        break;
    }

    if let Some(previous) = chosen {
        snapshot.escalations_before =
            culprit_core::EscalationBaseline::from_scan(zero_escalation_cover.unwrap_or(previous), snapshot);
        culprit_core::operations::compare(previous, snapshot);
        snapshot.contention_before =
            culprit_core::ContentionBaseline::from_scan(zero_contention_cover.unwrap_or(previous), snapshot);
    }
}

/// A saved scan's timeline from Culprit's scans built again from the counters the scans before it saved beside it in
/// `dir`, with this version's reading of each step (`trends::from_scans`): a step a saved scan misread (a statement
/// that outran the whole database, everything since a reset in one bucket) reads as the counters say, and a step they
/// can't account for in full stays marked so. The saved timeline stays where no earlier counters are kept, and only
/// counters files already saved are read: nothing is written.
pub fn rebuild_timeline(dir: &Path, snapshot: &mut culprit_core::Snapshot) {
    let evictions = trends::evictions(snapshot);
    let taken_at = snapshot.taken_at.clone();
    let Some(workload) = snapshot.workload.as_mut().filter(|w| {
        w.is_cumulative() && w.timeline.as_ref().is_none_or(|t| t.source == culprit_core::TimelineSource::Scans)
    }) else {
        return;
    };
    if workload.timeline.is_none() {
        // No captured timeline backs these derived references or query buckets.
        workload.history_inputs.retain(|r| r.purpose != culprit_core::BaselinePurpose::WorkloadTrend);
        for q in &mut workload.queries {
            q.trend = None;
        }
    }
    let Some(now) = trends::epoch(&taken_at) else { return };
    let oldest = now - trends::HISTORY_DAYS * 86_400.0;
    let history: Vec<Counters> = report_ids(dir)
        .into_iter()
        .filter(|id| id_epoch(id).is_some_and(|t| t >= oldest - 1.0 && t < now))
        .filter_map(|id| {
            fs::read(counters_path(dir, &id)).ok().and_then(|bytes| serde_json::from_slice::<Counters>(&bytes).ok())
        })
        .collect();
    if history.is_empty() {
        return;
    }
    let saved = workload.timeline.take();
    let trends: Vec<Option<culprit_core::QueryTrend>> = workload.queries.iter_mut().map(|q| q.trend.take()).collect();
    // from_scans replaces WorkloadTrend references only after finding comparable points. If it cannot rebuild,
    // the captured timeline, buckets and their retained references still describe the original observation.
    trends::from_scans(snapshot, evictions, &history);
    if let Some(workload) = snapshot.workload.as_mut().filter(|w| w.timeline.is_none()) {
        workload.timeline = saved;
        for (q, trend) in workload.queries.iter_mut().zip(trends) {
            q.trend = trend;
        }
    }
}

/// [`trends::evictions_baseline`] over the scans saved in `dir` (the last [`trends::HISTORY_DAYS`] days, a day or more
/// before `snapshot`), oldest first, stopping at the first the count continues: the oldest. Writes nothing.
pub fn evictions_baseline(dir: &Path, snapshot: &culprit_core::Snapshot) -> Option<culprit_core::CountBaseline> {
    trends::evictions(snapshot)?;
    let at = trends::epoch(&snapshot.taken_at)?;
    let newest = at - culprit_core::PER_DAY_MIN_HOURS * 3600.0;
    let oldest = at - trends::HISTORY_DAYS * 86_400.0;
    let mut history = Vec::new();
    for id in report_ids(dir) {
        // Ids are UTC timestamps to the millisecond, so a scan within a second of the cut is read and judged by its own.
        if id_epoch(&id).is_none_or(|t| t < oldest - 1.0 || t > newest + 1.0) {
            continue;
        }
        // A report without a counters file has none to give.
        let saved = fs::read(counters_path(dir, &id)).ok().and_then(|b| serde_json::from_slice::<Counters>(&b).ok());
        let Some(counters) = saved else { continue };
        history.push(counters);
        if let Some(baseline) = trends::evictions_baseline(&history, snapshot) {
            return Some(baseline);
        }
    }
    None
}

pub(crate) fn id_epoch(id: &str) -> Option<f64> {
    let t = chrono::NaiveDateTime::parse_from_str(id.get(..15)?, "%Y%m%dT%H%M%S").ok()?;
    Some(t.and_utc().timestamp() as f64)
}

pub fn latest_summary(dir: &Path, window_minutes: Option<u32>) -> Option<ReportSummary> {
    read_summary(&summary_path(dir, window_minutes))
}

/// A summary this version wrote, of a report it reads; `None` for one another version left behind.
fn read_summary(path: &Path) -> Option<ReportSummary> {
    let bytes = fs::read(path).ok()?;
    let mut summary: ReportSummary = serde_json::from_slice(&bytes).ok()?;
    if summary.format != culprit_core::SNAPSHOT_FORMAT {
        return None;
    }
    summary.render_messages();
    Some(summary)
}

/// The most recent report, or the most recent one taken with a given window (in minutes).
pub fn latest(dir: &Path, window_minutes: Option<u32>) -> Result<Option<Report>> {
    let Some(summary) = latest_summary(dir, window_minutes) else {
        return Ok(None);
    };
    load(&dir.join(format!("{}.json", summary.id))).map(Some)
}

pub fn load(path: &Path) -> Result<Report> {
    let bytes = fs::read(path).map_err(|e| Error::not_found(format!("{}: {e}", path.display())))?;
    storage::decode(&bytes).map_err(|e| {
        // `latest.json` is the one-line summary of the newest scan, not the scan itself.
        if serde_json::from_slice::<ReportSummary>(&bytes).is_ok() {
            return Error::invalid(culprit_core::t!("engine.reports.summary", file = path.display().to_string()))
                .with_hint(culprit_core::t!("engine.reports.summary_hint"));
        }
        not_a_report(path, &e)
    })
}

#[derive(Deserialize)]
struct WrappedSnapshot {
    snapshot: culprit_core::Snapshot,
}

#[derive(Deserialize)]
struct StoredHistoryScan {
    snapshot: culprit_core::Snapshot,
    #[serde(default, rename = "_culprit_store")]
    stored: Option<StoredRecordedChanges>,
}

#[derive(Deserialize)]
struct StoredRecordedChanges {
    recorded_changes: Vec<culprit_core::ChangeEvent>,
}

/// The prepared picture in a snapshot or report file, used to compare historical measurements.
/// Deserializes only `snapshot`, skipping findings and retained evidence differences directly in the JSON parser.
/// Analysis replay separately reads retained original input through `load_analysis_input`.
pub fn load_snapshot(path: &Path) -> Result<culprit_core::Snapshot> {
    let bytes = fs::read(path).map_err(|e| Error::not_found(format!("{}: {e}", path.display())))?;
    serde_json::from_slice::<WrappedSnapshot>(&bytes)
        .map(|w| w.snapshot)
        .or_else(|e| {
            if e.to_string().starts_with("missing field `snapshot`") {
                serde_json::from_slice::<culprit_core::Snapshot>(&bytes)
            } else {
                Err(e)
            }
        })
        .map(|mut snapshot| {
            snapshot.render_recorded_messages();
            snapshot
        })
        .map_err(|e| not_a_snapshot(path, &e))
}

/// `path` doesn't read as a report, for the reason `e` gives.
fn not_a_report(path: &Path, e: &dyn std::fmt::Display) -> Error {
    Error::invalid(culprit_core::t!(
        "engine.reports.not_a_report",
        file = path.display().to_string(),
        why = e.to_string()
    ))
}

/// `path` reads as neither a snapshot nor a report, for the reason `e` gives.
fn not_a_snapshot(path: &Path, e: &dyn std::fmt::Display) -> Error {
    Error::invalid(culprit_core::t!(
        "engine.reports.not_a_snapshot",
        file = path.display().to_string(),
        why = e.to_string()
    ))
}

/// Historical event backfills need only the prepared snapshot and any database-dated `recorded_changes`,
/// skipping findings and the retained `_culprit_store` snapshot diffs.
pub(crate) fn load_snapshot_and_recorded_changes(
    path: &Path,
) -> Result<(culprit_core::Snapshot, Vec<culprit_core::ChangeEvent>)> {
    let bytes = fs::read(path).map_err(|e| Error::not_found(format!("{}: {e}", path.display())))?;
    match serde_json::from_slice::<StoredHistoryScan>(&bytes) {
        Ok(scan) => {
            let recorded = scan.stored.map(|r| r.recorded_changes).unwrap_or_default();
            Ok((scan.snapshot, recorded))
        }
        Err(e) if e.to_string().starts_with("missing field `snapshot`") => {
            let snapshot =
                serde_json::from_slice::<culprit_core::Snapshot>(&bytes).map_err(|e| not_a_snapshot(path, &e))?;
            Ok((snapshot, Vec::new()))
        }
        Err(e) => Err(not_a_snapshot(path, &e)),
    }
}

/// Analysis reads retained input, while history's `load_snapshot` continues to read the scan's prepared picture. The
/// file is a saved report or a bare snapshot (`culprit analyze file.json`).
pub(crate) fn load_analysis_input(path: &Path) -> Result<crate::analysis::Input> {
    let bytes = fs::read(path).map_err(|e| Error::not_found(format!("{}: {e}", path.display())))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    if value.get("snapshot").is_some() && value.get("findings").is_some() && value.get("summary").is_some() {
        return storage::analysis_input(value).map_err(|e| not_a_report(path, &e));
    }
    let snapshot: culprit_core::Snapshot = serde_json::from_value(value).map_err(|e| not_a_snapshot(path, &e))?;
    Ok(crate::analysis::Input::from(snapshot))
}

pub fn list(dir: &Path) -> Result<Vec<ReportSummary>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<ReportSummary> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_report_file(p))
        .filter_map(|p| {
            let id = p.file_stem()?.to_str()?;
            read_summary(&p.with_file_name(format!("{id}.summary.json"))).filter(|s| s.id == id)
        })
        .collect();
    out.sort_by(|a, b| b.taken_at.cmp(&a.taken_at));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use culprit_core::SnapshotOptions;

    use super::*;

    #[test]
    fn plan_sample_provenance_is_saved_as_a_fact_and_worded_when_read() {
        use culprit_core::{PlanSampleSource, i18n::scope};
        let dir = std::env::temp_dir().join(format!("culprit-sample-language-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/mysql-demo.json")).unwrap();
        let query = snapshot.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.plan.is_some()).unwrap();
        let query_id = query.id.clone();
        query.plan.as_mut().unwrap().facts.sample_source = Some(PlanSampleSource::Digest);
        scope("fr", || save(&dir, &culprit_checks::analyze(snapshot.clone())).unwrap());
        let path = dir.join(format!("{}.json", snapshot.id));
        for language in ["en", "fr", "de", "es"] {
            scope(language, || {
                let restored = load_snapshot(&path).unwrap();
                let plan = restored.queries().iter().find(|q| q.id == query_id).unwrap().plan.as_ref().unwrap();
                assert_eq!(plan.facts.sample_source, Some(PlanSampleSource::Digest));
                assert_eq!(plan.props.get("Sample"), Some(&PlanSampleSource::Digest.sentence()));
            });
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_source_labels_render_for_the_reader_and_keep_counter_identity() {
        use culprit_core::i18n::scope;
        let dir = std::env::temp_dir().join(format!("culprit-source-language-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/yugabyte-demo.json")).unwrap();
        let workload = snapshot.workload.as_mut().unwrap();
        let source = workload.source.clone();
        let message = culprit_core::message!(
            "model.workload.source.ysql",
            scope = "node",
            node = "native-node",
            limited = "yes",
            role = "42"
        );
        workload.source_message = Some(message.clone());
        let history_source = culprit_core::message!(
            "model.observation.source.ysql",
            relation = "yb_pg_stat_plans",
            scope = "node",
            node = "native-node",
            limited = "no",
            role = ""
        );
        let initial_history_note = culprit_core::message!("provider.yugabyte.plan_history.note");
        let partial_history_note = culprit_core::message!("provider.yugabyte.plan_history.partial");
        let history_note =
            culprit_core::i18n::Message::paragraph(vec![initial_history_note.clone(), partial_history_note.clone()]);
        let query_id = workload.queries[0].id.clone();
        workload.queries[0].plan_history = Some(culprit_core::PlanHistory {
            source: String::new(),
            source_message: Some(history_source.clone()),
            note: String::new(),
            note_message: Some(initial_history_note),
            kind: culprit_core::PlanHistoryKind::Cache,
            captured_plan_id: None,
            captured_plan: None,
        });
        workload.queries[0].plan_history.as_mut().unwrap().append_note(partial_history_note);
        let activity_source = culprit_core::message!(
            "model.observation.source.ysql",
            relation = "yb_active_session_history",
            scope = "node",
            node = "native-node",
            limited = "yes",
            role = "42"
        );
        let weights = culprit_core::message!("provider.yugabyte.activity.weights");
        let visibility = culprit_core::message!("provider.yugabyte.terminations.role_visibility", role = "42");
        let resources_source = culprit_core::message!("provider.yugabyte.resources.source");
        let tracker_name = culprit_core::message!("provider.yugabyte.resources.root_tracker");
        let activity = snapshot.operations.activity_history.as_mut().unwrap();
        activity.period.source_message = Some(activity_source.clone());
        activity.period.note_messages = Some(vec![weights.clone()]);
        snapshot.operations.termination_history.as_mut().unwrap().visibility_message = Some(visibility.clone());
        let resources = snapshot.operations.node_resources.as_mut().unwrap();
        resources.source_message = Some(resources_source.clone());
        resources.nodes[0].process_memory.as_mut().unwrap().name_message = Some(tracker_name.clone());
        scope("fr", || save(&dir, &culprit_checks::analyze(snapshot.clone())).unwrap());
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(format!("{}.json", snapshot.id))).unwrap()).unwrap();
        assert_eq!(stored["snapshot"]["operations"]["activity_history"]["period"]["source"], "");
        assert_eq!(stored["snapshot"]["operations"]["activity_history"]["period"]["notes"], serde_json::json!([]));
        for language in ["en", "fr", "de", "es"] {
            scope(language, || {
                let restored = load_snapshot(&dir.join(format!("{}.json", snapshot.id))).unwrap();
                let workload = restored.workload.as_ref().unwrap();
                assert_eq!(workload.source, source);
                assert_eq!(workload.source_name(), message.render());
                assert_eq!(workload.source_label.as_deref(), Some(message.render().as_str()));
                let history =
                    workload.queries.iter().find(|q| q.id == query_id).unwrap().plan_history.as_ref().unwrap();
                assert_eq!(history.source, history_source.render());
                assert_eq!(history.note, history_note.render());
                let activity = restored.operations.activity_history.as_ref().unwrap();
                assert_eq!(activity.period.source, activity_source.render());
                assert_eq!(activity.period.notes, vec![weights.render()]);
                assert_eq!(restored.operations.termination_history.as_ref().unwrap().visibility, visibility.render());
                let resources = restored.operations.node_resources.as_ref().unwrap();
                assert_eq!(resources.source, resources_source.render());
                assert_eq!(resources.nodes[0].process_memory.as_ref().unwrap().name, tracker_name.render());
            });
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn compact_summaries_render_nested_cost_and_period_arguments_for_the_reader() {
        use culprit_core::i18n::scope;
        let dir = std::env::temp_dir().join(format!("culprit-summary-language-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/mssql-demo.json")).unwrap();
        let french = scope("fr", || culprit_checks::analyze(snapshot.clone()));
        save(&dir, &french).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(summary_path(&dir, None)).unwrap()).unwrap();
        assert_eq!(stored["headline"], "");
        assert!(stored["takeaway"].is_null());
        for language in ["en", "de", "fr", "es"] {
            scope(language, || {
                let expected = culprit_checks::analyze(snapshot.clone()).summary;
                let summary = latest_summary(&dir, None).unwrap();
                assert_eq!(summary.headline, expected.headline);
                assert_eq!(summary.takeaway, expected.takeaway);
                assert_eq!(summary.opportunities, expected.opportunities);
            });
        }
        // The count has its own card field; a count-only headline has no invented takeaway.
        let mut count_only = french.clone();
        count_only.summary.headline_message =
            Some(culprit_core::message!("summary.headline.opportunities", n = 1, count = "1"));
        count_only.summary.takeaway = None;
        count_only.summary.takeaway_message = None;
        let mut summary = summary_of(&count_only).for_storage();
        summary.render_messages();
        assert!(summary.takeaway.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_capability_reasons_and_actions_follow_the_reader_while_sql_stays_exact() {
        use culprit_core::i18n::scope;
        let dir = std::env::temp_dir().join(format!("culprit-capability-language-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../fixtures/postgres-demo.json")).unwrap();
        snapshot.workload = None;
        snapshot.server.capabilities.retain(|c| !c.is_workload_source());
        snapshot.server.missing.retain(|m| !m.capability.is_workload_source());
        let sql = "CREATE EXTENSION pg_stat_statements;";
        scope("fr", || {
            snapshot.server.missing.push(
                culprit_core::MissingCapability::new(
                    culprit_core::Capability::PgStatStatements,
                    culprit_core::message!("provider.postgres.capability.statements.not_installed"),
                )
                .with_fix(culprit_core::message!("provider.postgres.capability.statements.create", sql = sql))
                .with_action(sql, Some(culprit_core::message!("provider.postgres.capability.statements.preloaded"))),
            );
            save(&dir, &culprit_checks::analyze(snapshot.clone())).unwrap();
        });
        for language in ["en", "fr", "de"] {
            scope(language, || {
                let summary = latest_summary(&dir, None).unwrap();
                assert_eq!(
                    summary.no_workload,
                    Some(culprit_core::t!("provider.postgres.capability.statements.not_installed"))
                );
                let restored = load_snapshot(&dir.join(format!("{}.json", snapshot.id))).unwrap();
                let missing = restored.missing_workload().unwrap();
                assert_eq!(missing.reason, summary.no_workload.unwrap());
                assert_eq!(
                    missing.fix,
                    Some(culprit_core::t!("provider.postgres.capability.statements.create", sql = sql))
                );
                let action = missing.action.as_ref().unwrap();
                assert_eq!(action.sql, sql);
                assert_eq!(action.detail, Some(culprit_core::t!("provider.postgres.capability.statements.preloaded")));
            });
        }
        fs::remove_dir_all(dir).unwrap();
    }

    fn fixture_report(id: &str, window_minutes: u32) -> Report {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.report.json");
        let mut report: Report = serde_json::from_str(&fs::read_to_string(path).expect("fixture")).expect("report");
        report.snapshot.id = id.into();
        if let Some(at) = id_epoch(id) {
            report.snapshot.taken_at = culprit_core::format::iso_from_epoch(at);
        }
        report.snapshot.options = Some(SnapshotOptions::default().with_window_minutes(window_minutes));
        report
    }

    /// A connection scanned by another version keeps that version's files: its reports aren't read, so they are passed
    /// over as if absent (opening the connection scans; a digest has no scan before it), never an error.
    #[test]
    fn a_report_another_version_saved_is_passed_over() {
        let dir = std::env::temp_dir().join(format!("culprit-other-version-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let report = fixture_report("20261021T100000.000Z", 60);
        save(&dir, &report).unwrap();
        assert_eq!(latest_summary(&dir, None).map(|s| s.format), Some(culprit_core::SNAPSHOT_FORMAT));
        let summaries = [
            dir.join("latest.json"),
            dir.join("latest-1h.json"),
            dir.join(format!("{}.summary.json", report.snapshot.id)),
        ];
        for path in &summaries {
            let mut older: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            older.as_object_mut().unwrap().remove("format");
            fs::write(path, serde_json::to_vec(&older).unwrap()).unwrap();
        }
        assert!(latest_summary(&dir, None).is_none());
        assert!(latest_summary(&dir, Some(60)).is_none());
        assert!(list(&dir).unwrap().is_empty());
        assert!(latest(&dir, None).unwrap().is_none());
        // The next scan this version saves is the latest again.
        save(&dir, &fixture_report("20261021T110000.000Z", 60)).unwrap();
        assert_eq!(latest_summary(&dir, Some(60)).map(|s| s.id), Some("20261021T110000.000Z".to_string()));
        assert_eq!(list(&dir).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_rows_come_from_summary_sidecars_without_reading_reports() {
        let dir = std::env::temp_dir().join(format!("culprit-summary-header-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let report = fixture_report("20261021T100000.000Z", 60);
        save(&dir, &report).unwrap();
        assert_eq!(report_ids(&dir), vec![report.snapshot.id.clone()]);
        let path = dir.join(format!("{}.json", report.snapshot.id));
        let header = list(&dir).unwrap();
        assert_eq!(header[0].window_minutes, Some(60));
        // The report itself isn't read for its row.
        let mut value = serde_json::to_value(display(&report)).unwrap();
        value["findings"] = serde_json::json!("Not a typed finding array");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(list(&dir).unwrap()[0].id, header[0].id);
        // Without its summary, a report has no row.
        fs::remove_file(dir.join(format!("{}.summary.json", report.snapshot.id))).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_lock_baselines_use_only_earlier_matching_scans_and_write_nothing() {
        let dir = std::env::temp_dir().join(format!("culprit-lock-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let make = |id: &str, at: &str| {
            let mut r = fixture_report(id, 60);
            r.snapshot.taken_at = at.into();
            r.snapshot.server.started_at = Some("2026-10-01T00:00:00Z".into());
            r.snapshot.indexes[0].contention = Some(culprit_core::IndexContention {
                row_lock_waits: 1.0,
                row_lock_wait_ms: 13_000.0,
                ..Default::default()
            });
            r
        };
        let old = make("20261018T100000", "2026-10-18T10:00:00Z");
        save(&dir, &old).unwrap();
        let future = make("20261018T130000", "2026-10-18T13:00:00Z");
        save(&dir, &future).unwrap();
        let mut wrong = make("20261018T110000", "2026-10-18T11:00:00Z");
        wrong.snapshot.server.database = "another_database".into();
        save(&dir, &wrong).unwrap();
        let contents = || -> Vec<_> {
            let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
            files.sort();
            files
                .into_iter()
                .map(|p| {
                    let b = fs::read(&p).unwrap();
                    (p, b)
                })
                .collect()
        };
        let original = contents();
        let mut current = make("20261018T120000", "2026-10-18T12:00:00Z").snapshot;
        rebase_on_scans(&dir, &mut current);
        assert_eq!(current.contention_before.as_ref().unwrap().taken_at, old.snapshot.taken_at);
        assert_eq!(current.contention_before.as_ref().unwrap().since(&current.indexes[0]).unwrap().waits, 0.0);
        assert_eq!(contents(), original);
        // The nearest matching scan has an unknown start: do not leap over it to resurrect old counters.
        let mut unknown = make("20261018T113000", "2026-10-18T11:30:00Z");
        unknown.snapshot.server.started_at = None;
        save(&dir, &unknown).unwrap();
        rebase_on_scans(&dir, &mut current);
        assert!(current.contention_before.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_hours_scan_keeps_its_own_latest_pointer() {
        let dir = std::env::temp_dir().join(format!("culprit-reports-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        save(&dir, &fixture_report("20261013T080000", MINUTES_PER_DAY)).expect("save the day scan");
        save(&dir, &fixture_report("20261013T090000", 360)).expect("save the 6h scan");

        assert!(dir.join("latest-6h.json").exists());
        // The 6-hour scan doesn't take the 24-hour window's place.
        let id = |window: Option<u32>| latest(&dir, window).expect("read").expect("a report").snapshot.id;
        assert_eq!(id(Some(MINUTES_PER_DAY)), "20261013T080000");
        assert_eq!(id(Some(360)), "20261013T090000");
        assert_eq!(id(None), "20261013T090000");
        // An earlier scan that finishes later cannot replace the newer global or per-window pointer.
        save(&dir, &fixture_report("20261013T083000", 360)).unwrap();
        assert_eq!(id(None), "20261013T090000");
        assert_eq!(id(Some(360)), "20261013T090000");
        let summary = latest_summary(&dir, Some(360)).expect("6h summary");
        assert_eq!(summary.window_minutes, Some(360));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn latest_pointers_compare_scan_times_instead_of_id_spellings() {
        let dir = std::env::temp_dir().join(format!("culprit-reports-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let assert_latest = |expected: &str| {
            for window in [None, Some(60)] {
                assert_eq!(latest_summary(&dir, window).unwrap().id, expected);
                assert_eq!(latest(&dir, window).unwrap().unwrap().snapshot.id, expected);
            }
        };
        // A UTC ID without fractional digits sorts after every fractional ID in the same second.
        save(&dir, &fixture_report("20261021T100000Z", 60)).unwrap();
        save(&dir, &fixture_report("20261021T100000.500Z", 60)).unwrap();
        save(&dir, &fixture_report("20261021T100000.250Z", 60)).unwrap();
        assert_latest("20261021T100000.500Z");

        let mut newer = fixture_report("release-10", 60);
        newer.snapshot.taken_at = "2026-10-21T12:00:01+02:00".into();
        save(&dir, &newer).unwrap();
        let mut older = fixture_report("release-9", 60);
        older.snapshot.taken_at = "2026-10-21T10:00:00Z".into();
        save(&dir, &older).unwrap();
        assert_latest("release-10");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn historical_snapshot_loads_skip_findings_and_retained_diffs() {
        let dir = std::env::temp_dir().join(format!("culprit-snapshot-stream-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut report = fixture_report("20261018T100000", 60);
        let event = culprit_core::ChangeEvent::new(
            culprit_core::EventKind::Setting,
            culprit_core::EventSource::Database,
            "work_mem",
            "2026-10-18T09:30:00Z",
            "Setting work_mem changed",
        );
        report
            .evidence
            .get_or_insert_with(|| culprit_core::ReportEvidence {
                input: Box::new(report.snapshot.clone()),
                collected: None,
                recorded_changes: Vec::new(),
            })
            .recorded_changes = vec![event.clone()];
        save(&dir, &report).unwrap();
        let path = dir.join(format!("{}.json", report.snapshot.id));
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // Corrupt findings and retained input diff: full `load` fails, but `load_snapshot` and
        // `load_snapshot_and_recorded_changes` skip them in the JSON parser.
        value["findings"] = serde_json::json!("not-a-finding-array");
        value["_culprit_store"]["input"] = serde_json::json!("not-a-diff");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load(&path).is_err());
        let loaded = load_snapshot(&path).unwrap();
        assert_eq!(loaded.id, report.snapshot.id);
        let (loaded, recorded) = load_snapshot_and_recorded_changes(&path).unwrap();
        assert_eq!(loaded.id, report.snapshot.id);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].id, event.id);

        let bare_path = dir.join("bare-snapshot.json");
        fs::write(&bare_path, serde_json::to_vec(&report.snapshot).unwrap()).unwrap();
        assert_eq!(load_snapshot(&bare_path).unwrap().id, report.snapshot.id);
        let (bare, bare_recorded) = load_snapshot_and_recorded_changes(&bare_path).unwrap();
        assert_eq!(bare.id, report.snapshot.id);
        assert!(bare_recorded.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
