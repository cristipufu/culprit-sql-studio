use super::*;

#[test]
fn native_page_lookup_uses_the_same_bounded_resource_grammar_as_current_task_comparison() {
    let native = "6:1:372 (LATCH 0x0000000BF6759098: Class: BUFFER KP: 0 SH: 0 UP: 0 EX: 1 DT: 0 Sublatch: 0 HasWaiters: 1 Task: 0x0000000B96434108 AnyReleasor: 0)";
    let expected = PageAddress { database: 6, file: 1, page: 372 };
    assert_eq!(PageAddress::parse(native), Some(expected));
    assert_eq!(PageAddress::parse("6:1:372"), Some(expected));
    assert_eq!(
        culprit_core::telemetry::PageWaitAddress::parse_sql_server_resource(native),
        Some(culprit_core::telemetry::PageWaitAddress { database_id: 6, file_id: 1, page_id: 372 })
    );
    for invalid in ["6:1:372 (LATCH unknown)", "32768:1:372", "6:1:2147483648", "6:1:372:0"] {
        assert!(PageAddress::parse(invalid).is_none(), "{invalid}");
    }
}
