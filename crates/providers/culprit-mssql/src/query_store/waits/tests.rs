use super::*;
use crate::value::Val;
use culprit_core::telemetry::{MeasurementValue, StatementReferences, TelemetryRecord, TelemetryStream};

fn summary(id: u8, name: &str, milliseconds: u64) -> Summary {
    Summary { plan_id: None, category_id: id, category: name.into(), total_ms: milliseconds }
}

#[test]
fn documented_native_id_name_and_policy_have_one_owner_for_scans_and_capture() {
    let metadata = semantics(WaitCaptureState::Enabled);
    assert!(metadata.is_valid());
    assert_eq!(metadata.categories.len(), 24);
    let mut distinct_ids = BTreeSet::new();
    for category in CATEGORIES {
        assert!(distinct_ids.insert(category.id));
        let meaning = &metadata.categories[&category.id.to_string()];
        assert_eq!(meaning.native_name, category.native_name);
        assert_eq!(meaning.disposition, category.disposition);
        let rows = [summary(category.id, category.native_name, 7)];
        let actual = fold(&rows);
        match meaning.disposition {
            WaitDisposition::Analyze(class) => {
                assert_eq!(actual.values.len(), 1);
                assert_eq!(actual.values[0].category, class.category());
                assert_eq!(actual.values[0].wait_type.as_deref(), Some(meaning.native_name.as_str()));
                assert_eq!(actual.values[0].wait_ms, 7.0);
                assert!(actual.unknown_categories.is_empty());
            }
            WaitDisposition::Ignore => {
                assert!(actual.values.is_empty() && actual.unknown_categories.is_empty());
            }
            WaitDisposition::Unknown => {
                assert!(actual.values.is_empty());
                assert_eq!(actual.unknown_categories.len(), 1);
            }
        }
    }
    assert_eq!(metadata.categories["1"].disposition, WaitDisposition::Analyze(WaitClass::Cpu));
    assert_eq!(metadata.categories["2"].disposition, WaitDisposition::Analyze(WaitClass::Workers));
    assert!(!coverage(true).classes.contains(&WaitClass::Rpc));
}

#[test]
fn folds_known_waits_without_idle_time_or_a_worker_wait_becoming_cpu_consumption() {
    let rows = [
        summary(6, "Buffer IO", 10),
        summary(21, "Other Disk IO", 5),
        summary(1, "CPU", 20),
        summary(2, "Worker Thread", 30),
        summary(11, "Idle", 1_000),
    ];
    let folded = fold(&rows);
    assert!(folded.unknown_categories.is_empty());
    assert_eq!(folded.values.len(), 3);
    assert_eq!(folded.values[0].category, "Workers");
    assert_eq!(folded.values[1].category, "CPU");
    assert_eq!(folded.values[1].wait_ms, 20.0);
    assert_eq!(folded.values[2].category, "IO");
    assert_eq!(folded.values[2].wait_ms, 15.0);
    assert_eq!(folded.values[2].wait_type.as_deref(), Some("Buffer IO"));
}

#[test]
fn unknown_ids_names_and_native_unknown_are_explicit_without_inventing_other_or_zero_rows() {
    let rows = [summary(250, "Lock", 40), summary(3, "Memory", 50), summary(0, "Unknown", 0), summary(3, "Lock", 10)];
    let folded = fold(&rows);
    assert_eq!(folded.unknown_categories, ["0:Unknown".into(), "250:Lock".into(), "3:Memory".into()].into());
    assert_eq!(folded.values.len(), 1);
    assert_eq!(folded.values[0].category, "Lock");
    assert_eq!(folded.values[0].wait_ms, 10.0);
    assert!(!semantics(WaitCaptureState::Enabled).categories.contains_key("250"));
    assert!(fold([]).values.is_empty());
    assert!(coverage(true).complete);
    assert!(!coverage(false).complete);
    assert!(coverage(true).classes.contains(&WaitClass::Network));
}

#[test]
fn capture_is_frozen_from_the_native_settings_value_without_treating_unknown_as_disabled() {
    for (native, expected) in [("ON", WaitCaptureState::Enabled), ("OFF", WaitCaptureState::Disabled)] {
        let row = Rec::of(&[("wait_stats_capture_mode_desc", Val::Text(native.into()))]);
        let state = capture_state(&row).unwrap();
        assert_eq!(state, expected);
        let metadata = semantics(state);
        assert!(metadata.is_valid());
        assert_eq!(metadata.capture, state);
    }
    assert!(capture_state(&Rec::default()).is_err());
    for value in [Val::Null, Val::Text("UNKNOWN".into()), Val::Text("on".into()), Val::Int(1)] {
        assert!(capture_state(&Rec::of(&[("wait_stats_capture_mode_desc", value)])).is_err());
    }
}

#[test]
fn unknown_raw_categories_survive_export_while_invalid_native_id_and_time_are_rejected() {
    let row = Rec::of(&[
        ("row_id", Val::Int(1)),
        ("plan_id", Val::Int(2)),
        ("replica_group_id", Val::Int(0)),
        ("execution_type", Val::Int(0)),
        ("wait_category", Val::Int(250)),
        ("total_query_wait_time_ms", Val::Int(40)),
        ("avg_query_wait_time_ms", Val::Float(20.0)),
        ("min_query_wait_time_ms", Val::Int(10)),
        ("max_query_wait_time_ms", Val::Int(30)),
    ]);
    let TelemetryRecord::QueryWaits(wait) =
        super::super::decode::record(&row, TelemetryStream::QueryWaits, StatementReferences::default()).unwrap()
    else {
        panic!("native wait record")
    };
    assert_eq!(wait.category, "250");
    assert_eq!(wait.total, MeasurementValue::Integer("40".into()));
    for id in [-1, 256] {
        let row = Rec::of(&[
            ("wait_category", Val::Int(id)),
            ("wait_category_desc", Val::Text("Lock".into())),
            ("wait_ms", Val::Int(10)),
        ]);
        assert!(Summary::decode(&row, false).is_err());
    }
}
