use super::*;
use culprit_core::{SnapshotOptions, telemetry::NativeIdentity};
use std::path::{Path, PathBuf};

fn save(paths: &Paths, at: &str, population: Option<NativeIdentity>) -> (PathBuf, Report) {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.report.json");
    let mut report: Report = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    report.snapshot.id = at.replace(['-', ':'], "");
    report.snapshot.taken_at = at.into();
    report.snapshot.options =
        Some(SnapshotOptions { workload_population: population.clone(), ..SnapshotOptions::default() });
    report.snapshot.workload.as_mut().unwrap().population = population;
    let directory = paths.reports_dir("shop");
    reports::save(&directory, &report).unwrap();
    (directory.join(format!("{}.json", report.snapshot.id)), report)
}

fn paths(home: &Path) -> Paths {
    Paths { config: home.join("config"), data: home.join("data") }
}

#[test]
fn default_digest_keeps_its_anchor_and_baseline_across_interleaved_selected_scans() {
    let home = tempfile::tempdir().unwrap();
    let paths = paths(home.path());
    save(&paths, "2026-11-01T10:00:00Z", None);
    save(&paths, "2026-11-01T11:00:00Z", Some(NativeIdentity::new("7")));
    save(&paths, "2026-11-01T12:00:00Z", None);
    save(&paths, "2026-11-01T13:00:00Z", Some(NativeIdentity::new("18")));
    let service = Reports { paths: &paths, checks: &[] };
    let latest = service.latest_report("shop", None).unwrap().unwrap();
    let digest = service.digest(Some("shop"), None, None, 5, 5).unwrap();
    assert_eq!(latest.snapshot.taken_at, "2026-11-01T12:00:00Z");
    assert_eq!(digest.taken_at, latest.snapshot.taken_at);
    assert_eq!(digest.since.unwrap().taken_at, "2026-11-01T10:00:00Z");
    assert!(digest.measurement.unwrap().population.is_none());
}

#[test]
fn selected_reports_need_an_explicit_file_when_no_default_digest_exists() {
    let home = tempfile::tempdir().unwrap();
    let paths = paths(home.path());
    let (path, report) = save(&paths, "2026-11-01T11:00:00Z", Some(NativeIdentity::new("7")));
    let service = Reports { paths: &paths, checks: &[] };
    assert!(service.latest_report("shop", None).unwrap().is_none());
    let error = service.digest(Some("shop"), None, None, 5, 5).unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::NotFound);
    let explicit = service.digest(Some("shop"), Some(&path), None, 5, 5).unwrap();
    assert_eq!(explicit.taken_at, report.snapshot.taken_at);
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    assert_eq!(explicit.measurement.unwrap().population, shared.workload.unwrap().population);
    assert!(explicit.since.is_none());
}
