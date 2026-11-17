use super::*;
use culprit_core::{ResourceSample, telemetry::DataIoAccounting};

fn observed(accounting: DataIoAccounting) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.options = None;
    let start = format::epoch_seconds("2026-10-11T10:00:15Z").unwrap();
    snapshot.resources = (0..240)
        .map(|index| ResourceSample {
            data_io_accounting: accounting,
            at: format::iso_from_epoch(start + f64::from(index) * 15.0),
            data_io_pct: Some(99.0),
            cpu_pct: None,
            log_write_pct: None,
            memory_pct: None,
            workers_pct: None,
        })
        .collect();
    snapshot
}

#[test]
fn the_same_native_percent_has_accounting_specific_wording_without_remote_pressure_or_disk_claims() {
    for accounting in [DataIoAccounting::DataFiles, DataIoAccounting::ComputeLocalSsd] {
        let mut snapshot = observed(accounting);
        health::grade_on_evidence(&mut snapshot);
        let findings = tier_limit(&snapshot);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "tier-limit:data-io", "one stable subject across known native bases");
        assert!(findings[0].title.starts_with(&accounting.label()));
        assert!(findings[0].summary.contains(&accounting.meaning()));
        let signal = snapshot.pressure.as_ref().unwrap().signal("data-io").unwrap();
        assert_eq!(signal.label, accounting.label());
        assert!(signal.healthy.as_ref().unwrap().contains(&accounting.meaning()));
        if accounting == DataIoAccounting::ComputeLocalSsd {
            assert!(findings[0].summary.contains("Page-server I/O is excluded"));
            assert!(!findings[0].title.contains("Data reads"));
            assert!(!findings[0].fix.as_ref().unwrap().summary.contains("missing-index"));
        }
    }
}

#[test]
fn unknown_or_incompatible_source_bases_retain_native_values_without_a_comparable_limit_finding() {
    let mut unknown = observed(DataIoAccounting::Unknown);
    assert!(tier_limit(&unknown).is_empty());
    health::grade_on_evidence(&mut unknown);
    let signal = unknown.pressure.as_ref().unwrap().signal("data-io").unwrap();
    assert_eq!(signal.status, SignalStatus::Info);
    assert!(signal.healthy.as_ref().unwrap().contains(&DataIoAccounting::Unknown.meaning()));
    let mut mixed = observed(DataIoAccounting::DataFiles);
    mixed.resources[0].data_io_accounting = DataIoAccounting::ComputeLocalSsd;
    assert!(tier_limit(&mixed).is_empty());
    health::grade_on_evidence(&mut mixed);
    assert!(mixed.pressure.as_ref().unwrap().signal("data-io").is_none());
    assert_eq!(mixed.resources.len(), 240);
    assert!(mixed.resources.iter().all(|sample| sample.data_io_pct == Some(99.0)));
}

#[test]
fn low_local_utilization_does_not_prove_the_absence_of_remote_page_server_work() {
    let mut snapshot = observed(DataIoAccounting::ComputeLocalSsd);
    for sample in &mut snapshot.resources {
        sample.data_io_pct = Some(0.0);
    }
    assert!(tier_limit(&snapshot).is_empty());
    health::grade_on_evidence(&mut snapshot);
    let signal = snapshot.pressure.as_ref().unwrap().signal("data-io").unwrap();
    assert_eq!(signal.value, 0.0);
    assert!(signal.healthy.as_ref().unwrap().contains("Page-server I/O is excluded"));
}

#[test]
fn unqualified_data_io_never_withholds_independently_measured_cpu_log_or_worker_limits() {
    for mixed in [false, true] {
        let mut snapshot = observed(if mixed { DataIoAccounting::DataFiles } else { DataIoAccounting::Unknown });
        if mixed {
            snapshot.resources[0].data_io_accounting = DataIoAccounting::ComputeLocalSsd;
        }
        for sample in &mut snapshot.resources {
            sample.cpu_pct = Some(99.0);
            sample.log_write_pct = Some(99.0);
            sample.workers_pct = Some(99.0);
        }
        let original = serde_json::to_value(&snapshot.resources).unwrap();
        let findings = tier_limit(&snapshot);
        let subjects = findings.iter().map(|finding| finding.id.as_str()).collect::<Vec<_>>();
        assert_eq!(subjects, ["tier-limit:log-write", "tier-limit:cpu", "tier-limit:workers"]);
        assert_eq!(
            serde_json::to_value(&snapshot.resources).unwrap(),
            original,
            "eligibility never repairs or removes original native samples"
        );
    }
}
