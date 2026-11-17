use super::*;
use crate::maintenance_blocking_report::maintenance_blocking_focus;

fn sources(qualified: bool) -> (CollectionDescription, CollectionDescription) {
    let (blocking, closure, _) = fixture(qualified);
    let mut maintenance = blocking.clone();
    maintenance.source = "controlled-maintenance".into();
    maintenance.observation = SourceObservation::MaintenanceProgress {
        snapshot: StateSnapshotDescription {
            dataset: SnapshotDataset::MaintenanceProgress,
            observed_from: "2026-11-13T11:00:00Z".into(),
            observed_until: "2026-11-13T11:00:00.1Z".into(),
            records: 0,
            sources: vec![StateSourceCoverage::observed("controlled-progress".into(), 0, false)],
        },
        result: Box::new(FocusedRequestResult {
            focus: Some(Box::new(closure.focus)),
            database_writable: Some(true),
            binding: closure.binding,
            membership: FocusedRequestMembership::NotObserved,
        }),
    };
    assert!(maintenance.partition_is_consistent());
    (maintenance, blocking)
}

#[test]
fn exact_original_receipts_link_source_headers_without_upgrading_unknown_original_members() {
    for qualified in [false, true] {
        let (earlier, later) = sources(qualified);
        let focus = maintenance_blocking_focus(&earlier, &later).unwrap();
        let SourceObservation::FocusedBlocking { result, .. } = &later.observation else { panic!() };
        assert_eq!(focus, &result.focus);
        assert_eq!(focus.partition == SourcePartition::Target, !qualified);
        assert_eq!(result.binding == FocusedRequestBinding::UnverifiedOriginalScope, !qualified);
    }
}

#[test]
fn matching_request_fields_do_not_replace_an_exact_original_receipt_or_current_native_scope() {
    let (earlier, later) = sources(false);
    for field in ["parent", "member", "epoch", "target", "independent"] {
        let mut changed = earlier.clone();
        match field {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    panic!()
                };
                member.native.id = "different-current-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = Some("different-current-generation".into());
            }
            "target" => {
                changed.target = crate::DatabaseIdentity::server(Engine::Mssql, "other", "db", 1433, None).into()
            }
            _ => {
                let SourceObservation::MaintenanceProgress { result, .. } = &mut changed.observation else { panic!() };
                if field == "independent" {
                    result.focus = None;
                    result.binding = FocusedRequestBinding::Independent;
                } else {
                    result.focus.as_mut().unwrap().parent.manifest_sha256 = digest(b"different original capture proof");
                }
            }
        }
        assert!(maintenance_blocking_focus(&changed, &later).is_err(), "{field}");
    }
}

#[test]
fn native_order_keeps_nanosecond_precision_and_never_uses_a_delivery_clock() {
    let (mut earlier, mut later) = sources(true);
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut later.observation else { panic!() };
    snapshot.at = "2026-11-13T11:00:00.100000010Z".into();
    for (until, accepted) in [
        ("2026-11-13T11:00:00.100000009Z", true),
        ("2026-11-13T11:00:00.100000011Z", false),
        ("not a native timestamp", false),
        ("", false),
    ] {
        let SourceObservation::MaintenanceProgress { snapshot, .. } = &mut earlier.observation else { panic!() };
        snapshot.observed_until = until.into();
        assert_eq!(maintenance_blocking_focus(&earlier, &later).is_ok(), accepted, "{until}");
    }
}
