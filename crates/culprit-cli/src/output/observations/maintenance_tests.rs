use super::*;
use culprit_collector::analytical::test_support::maintenance_activity_evidence;
use culprit_core::{MaintenanceDirection, MaintenanceReport, telemetry::*};

fn report(direction: MaintenanceDirection) -> MaintenanceReport {
    let (source, rows) = maintenance_activity_evidence(direction);
    let observations = rows
        .iter()
        .map(|row| {
            let TelemetryRecord::Activity(observation) = row else { panic!() };
            ActivityFrameRow { observation, reference: None }
        })
        .collect::<Vec<_>>();
    culprit_checks::maintenance::explain(&source, &observations, true).unwrap()
}

#[test]
fn maintenance_cli_preserves_canonical_actions_for_blocking_affected_mixed_and_unlinked_requests() {
    for direction in [
        MaintenanceDirection::BlockingSession,
        MaintenanceDirection::Waiting,
        MaintenanceDirection::Both,
        MaintenanceDirection::Unlinked,
    ] {
        let report = report(direction);
        assert_eq!(report.entries[0].direction, direction);
        let text = maintenance(&report);
        for entry in &report.entries {
            for field in [&entry.title, &entry.explanation, &entry.investigation, &entry.verify] {
                assert!(text.contains(&super::super::brief_detail(field, OBSERVATION_DETAIL_CHARS)), "{field}");
            }
        }
        if let Some(limitation) = report.limitations.first() {
            assert!(text.contains(limitation), "the shared brief keeps its leading qualification");
        }
        for absent in ["ALTER INDEX", "SELECT payload", "UPDATE dbo", "9264", "native-statement-", "payload_sha256"] {
            assert!(!text.contains(absent), "{absent}");
        }
        let restored: MaintenanceReport = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(maintenance(&restored), text);
    }
}

#[test]
fn a_long_maintenance_explanation_cannot_hide_independently_bounded_investigation_and_verification() {
    let mut report = report(MaintenanceDirection::Both);
    let entry = &mut report.entries[0];
    let investigation = entry.investigation.clone();
    let verify = entry.verify.clone();
    entry.explanation = "é".repeat(4096);
    let text = maintenance(&report);
    assert!(!text.contains(&"é".repeat(4096)));
    assert!(text.chars().filter(|character| *character == 'é').count() <= OBSERVATION_DETAIL_CHARS);
    assert!(text.contains(&investigation));
    assert!(text.contains(&verify));
}

#[test]
fn maintenance_entries_keep_the_shared_three_entry_and_per_field_unicode_limits() {
    let mut report = report(MaintenanceDirection::Both);
    let entry = report.entries[0].clone();
    report.entries = (0..20)
        .map(|index| {
            let mut entry = entry.clone();
            entry.title = format!("maintenance-{index}");
            entry.explanation = "é".repeat(4096);
            entry.investigation = "λ".repeat(4096);
            entry.verify = "界".repeat(4096);
            entry
        })
        .collect();
    let text = maintenance(&report);
    assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
    for character in ['é', 'λ', '界'] {
        assert_eq!(
            text.chars().filter(|value| *value == character).count(),
            super::super::BRIEF_FINDING_LIMIT * OBSERVATION_DETAIL_CHARS
        );
    }
    assert!(!text.contains("maintenance-3"));
}
