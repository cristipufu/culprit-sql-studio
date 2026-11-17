use super::*;

const NATIVE: &str = "6:1:372 (LATCH 0x0000000BF6759098: Class: BUFFER KP: 0 SH: 0 UP: 0 EX: 1 DT: 0 Sublatch: 0 HasWaiters: 1 Task: 0x0000000B96434108 AnyReleasor: 0)";

fn resource(native: &str) -> ObservedWaitResource {
    ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new(native).qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
    }
}

#[test]
fn bare_coordinates_and_the_observed_buffer_latch_diagnostic_identify_only_the_page() {
    let expected = PageWaitAddress { database_id: 6, file_id: 1, page_id: 372 };
    for native in ["6:1:372", "0000000006:0000000001:0000000372", NATIVE] {
        assert_eq!(PageWaitAddress::parse_sql_server_resource(native), Some(expected), "{native}");
    }
    assert_eq!(
        PageWaitAddress::parse_sql_server_resource("32767:2147483647:2147483647"),
        Some(PageWaitAddress { database_id: 32767, file_id: 2147483647, page_id: 2147483647 })
    );
    assert_eq!(PageWaitAddress::parse_sql_server_resource("6:1:0").unwrap().page_id, 0);
    assert!(PageWaitAddress::parse_sql_server_resource(&NATIVE.replace("EX: 1", "EX: 18446744073709551615")).is_some());
}

#[test]
fn changing_diagnostic_task_addresses_and_flags_does_not_change_physical_coordinates_or_raw_evidence() {
    let original = resource(NATIVE);
    let changed = resource(
        &NATIVE
            .replace("0x0000000BF6759098", "0x0000000000000000")
            .replace("EX: 1", "EX: 0")
            .replace("HasWaiters: 1", "HasWaiters: 0")
            .replace("Task: 0x0000000B96434108", "Task: 0x0000000000000000"),
    );
    assert!(original.same_sql_server_page(&changed));
    assert!(original.same_sql_server_page(&resource("6:1:372")));
    assert_ne!(original.native.id, changed.native.id);
    assert_eq!(original.native.id, NATIVE, "no suffix text is discarded from the native fact");
    assert_eq!(
        serde_json::from_str::<ObservedWaitResource>(&serde_json::to_string(&original).unwrap()).unwrap(),
        original
    );
}

#[test]
fn coordinates_never_cross_wait_kinds_native_provenance_or_different_pages() {
    let original = resource(NATIVE);
    for native in ["5:1:372", "6:2:372", "6:1:373"] {
        assert!(!original.same_sql_server_page(&resource(native)), "{native}");
    }
    let mut other = resource("6:1:372");
    other.kind = WaitResourceKind::PageIo;
    assert!(!original.same_sql_server_page(&other));
    other.kind = WaitResourceKind::DataLock;
    assert!(!other.same_sql_server_page(&other));
    other.kind = original.kind;
    other.native.qualifiers.insert("source".into(), "sys.dm_exec_requests.wait_resource".into());
    assert!(!original.same_sql_server_page(&other));
    other = resource("6:1:372");
    other.native.qualifiers.insert("native-scope".into(), "different".into());
    assert!(!original.same_sql_server_page(&other));
}

#[test]
fn malformed_or_out_of_range_coordinates_do_not_gain_authority_from_a_suffix() {
    for coordinates in [
        "",
        "0:1:1",
        "6:0:1",
        "32768:1:1",
        "6:2147483648:1",
        "6:1:2147483648",
        "6:1:4294967296",
        "6:1:-1",
        "+6:1:1",
        "6: 1:1",
        "6:1:1:2",
        "6::1",
        "6:1:1.0",
        "6:1:1e1",
        "6:1:١",
        "00000000006:1:1",
    ] {
        for native in [coordinates.to_string(), NATIVE.replacen("6:1:372", coordinates, 1)] {
            assert!(PageWaitAddress::parse_sql_server_resource(&native).is_none(), "{native}");
        }
    }
}

#[test]
fn unsupported_or_partial_diagnostics_never_fall_back_to_prefix_or_raw_text_equality() {
    let mut invalid = vec![
        NATIVE.replace("Class: BUFFER", "Class: OTHER"),
        NATIVE.replace("KP: 0 SH: 0", "SH: 0 KP: 0"),
        NATIVE.replace("Task: 0x0000000B96434108 ", ""),
        NATIVE.replace("Task: 0x0000000B96434108", "Task: 0x1234"),
        NATIVE.replace("LATCH 0x0000000BF6759098", "LATCH 0x0000000BF675909Z"),
        NATIVE.replace("EX: 1", "EX: 18446744073709551616"),
        NATIVE.replace("EX: 1", "EX: -1"),
        NATIVE.replace("EX: 1", "EX: 1.0"),
        NATIVE.replace("KP: 0", "KP:  0"),
        NATIVE.trim_end_matches(')').into(),
        format!("{NATIVE} trailing"),
        NATIVE.replace("AnyReleasor: 0)", "AnyReleasor: 0 Extra: 1)"),
        "6:1:372 (LATCH unknown)".into(),
        "6:1:372 (Task: 0x0000000000000001)".into(),
    ];
    invalid.push(format!("{NATIVE}{}", " ".repeat(512)));
    for native in invalid {
        assert!(PageWaitAddress::parse_sql_server_resource(&native).is_none(), "{native}");
        let native = resource(&native);
        assert!(!native.same_sql_server_page(&native), "unsupported identical text stays unqualified");
    }
}
