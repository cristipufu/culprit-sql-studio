use super::*;

fn point<'a>(thread: &'a str, values: [Option<&'a str>; 4]) -> PlanPageServerThread<'a> {
    let [reads, read_aheads, lob_reads, lob_read_aheads] = values;
    PlanPageServerThread { thread: Some(thread), reads, read_aheads, lob_reads, lob_read_aheads }
}
fn recorded() -> PlanPageServerIo {
    let mut read = PlanPageServerIoBuilder::default();
    read.observe(point("0", [Some("9007199254740993"), Some("0"), Some("18446744073709551615"), Some("3")]));
    read.finish().unwrap()
}

#[test]
fn exact_native_pages_keep_zero_read_ahead_lob_and_the_unsigned_boundary_separate() {
    let recorded = recorded();
    assert!(recorded.is_valid());
    assert_eq!(recorded.threads_observed, 1);
    assert_eq!(recorded.reads.as_deref(), Some("9007199254740993"));
    assert_eq!(recorded.read_aheads.as_deref(), Some("0"));
    assert_eq!(recorded.lob_reads.as_deref(), Some("18446744073709551615"));
    assert_eq!(recorded.lob_read_aheads.as_deref(), Some("3"));
    assert!(recorded.issues.is_empty());
}

#[test]
fn complete_unique_native_threads_sum_only_the_same_operators_corresponding_fields() {
    let mut read = PlanPageServerIoBuilder::default();
    read.observe(point("0", [Some("0"), Some("0"), Some("0"), Some("0")]));
    read.observe(point("1", [Some("9007199254740993"), Some("2"), Some("3"), Some("4")]));
    read.observe(point("2", [Some("2"), Some("5"), Some("7"), Some("11")]));
    let value = read.finish().unwrap();
    assert_eq!(value.reads.as_deref(), Some("9007199254740995"));
    assert_eq!(value.read_aheads.as_deref(), Some("7"));
    assert_eq!(value.lob_reads.as_deref(), Some("10"));
    assert_eq!(value.lob_read_aheads.as_deref(), Some("15"));
    assert_eq!(value.threads_observed, 3);
}

#[test]
fn ubiquitous_absent_attributes_create_neither_zeroes_nor_a_warning() {
    let mut read = PlanPageServerIoBuilder::default();
    read.observe(point("0", [None; 4]));
    read.observe(point("1", [None; 4]));
    assert!(read.finish().is_none());
}

#[test]
fn a_missing_thread_field_withholds_its_total_and_preserves_other_independent_fields() {
    let mut read = PlanPageServerIoBuilder::default();
    read.observe(point("1", [None, Some("2"), Some("3"), Some("0")]));
    read.observe(point("2", [Some("5"), Some("7"), Some("11"), Some("0")]));
    let value = read.finish().unwrap();
    assert!(value.is_valid());
    assert_eq!(value.reads, None);
    assert_eq!(value.read_aheads.as_deref(), Some("9"));
    assert_eq!(value.lob_reads.as_deref(), Some("14"));
    assert_eq!(value.lob_read_aheads.as_deref(), Some("0"));
    assert_eq!(value.issues, [PlanPageServerIoIssue::MissingCounter { metric: PlanPageServerMetric::Reads }]);
}

#[test]
fn malformed_native_fields_never_become_zero_or_retained_free_form_text() {
    for invalid in ["-1", "1.5", "NaN", "Infinity", "18446744073709551616", "private-native-value", "01"] {
        let mut read = PlanPageServerIoBuilder::default();
        read.observe(point("0", [Some(invalid), Some("1"), None, Some("0")]));
        let value = read.finish().unwrap();
        assert!(value.is_valid(), "{invalid}");
        assert!(value.reads.is_none());
        assert_eq!(value.read_aheads.as_deref(), Some("1"));
        assert!(value.issues.contains(&PlanPageServerIoIssue::InvalidCounter { metric: PlanPageServerMetric::Reads }));
        assert!(!serde_json::to_string(&value).unwrap().contains("private-native-value"));
    }
}

#[test]
fn invalid_duplicate_and_ambiguous_thread_populations_withhold_all_totals() {
    for invalid in [None, Some("-1"), Some("2147483648"), Some("private-thread")] {
        let mut read = PlanPageServerIoBuilder::default();
        let mut native = point("0", [Some("1"); 4]);
        native.thread = invalid;
        read.observe(native);
        let value = read.finish().unwrap();
        assert!(value.is_valid());
        assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| value.value(metric).is_none()));
        assert_eq!(value.issues, [PlanPageServerIoIssue::InvalidThread]);
    }
    let mut duplicate = PlanPageServerIoBuilder::default();
    duplicate.observe(point("1", [Some("1"); 4]));
    duplicate.observe(point("1", [Some("1"); 4]));
    let value = duplicate.finish().unwrap();
    assert!(value.is_valid());
    assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| value.value(metric).is_none()));
    assert_eq!(value.issues, [PlanPageServerIoIssue::DuplicateThread]);

    let mut ambiguous = PlanPageServerIoBuilder::default();
    ambiguous.observe(point("0", [Some("1"); 4]));
    ambiguous.mark_multiple_populations();
    let value = ambiguous.finish().unwrap();
    assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| value.value(metric).is_none()));
    assert_eq!(value.issues, [PlanPageServerIoIssue::MultiplePopulations]);
}

#[test]
fn checked_addition_withholds_only_the_overflowing_counter() {
    let mut read = PlanPageServerIoBuilder::default();
    read.observe(point("1", [Some("18446744073709551615"), Some("0"), Some("2"), Some("3")]));
    read.observe(point("2", [Some("1"), Some("0"), Some("2"), Some("3")]));
    let value = read.finish().unwrap();
    assert!(value.is_valid());
    assert_eq!(value.reads, None);
    assert_eq!(value.read_aheads.as_deref(), Some("0"));
    assert_eq!(value.lob_reads.as_deref(), Some("4"));
    assert_eq!(value.issues, [PlanPageServerIoIssue::CounterOverflow { metric: PlanPageServerMetric::Reads }]);
}

#[test]
fn native_thread_policy_withholds_oversized_population_without_retaining_a_vector_or_partial_sum() {
    let mut read = PlanPageServerIoBuilder::default();
    for thread in 0..=MAX_PLAN_SCAN_THREADS {
        read.observe(point(&thread.to_string(), [Some("1"); 4]));
    }
    let value = read.finish().unwrap();
    assert!(value.is_valid());
    assert_eq!(value.threads_observed as usize, MAX_PLAN_SCAN_THREADS);
    assert_eq!(value.issues, [PlanPageServerIoIssue::ThreadLimit]);
    assert!(PlanPageServerMetric::ALL.into_iter().all(|metric| value.value(metric).is_none()));
}

#[test]
fn saved_runtime_validation_rejects_repaired_totals_noncanonical_counts_and_duplicate_issues() {
    let original = recorded();
    let runtime = crate::NodeRuntime { page_server_io: Some(Box::new(original.clone())), ..Default::default() };
    let json = serde_json::to_value(&runtime).unwrap();
    let decoded: crate::NodeRuntime = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(runtime, decoded);
    for invalid in ["private-saved-counter", "1.1", "18446744073709551616", "01"] {
        let mut changed = json.clone();
        changed["page_server_io"]["reads"] = invalid.into();
        assert!(serde_json::from_value::<crate::NodeRuntime>(changed).is_err());
    }
    let mut changed = original;
    changed.issues.push(PlanPageServerIoIssue::DuplicateThread);
    assert!(!changed.is_valid(), "an ambiguous population cannot keep numerical totals");
    changed.reads = None;
    changed.read_aheads = None;
    changed.lob_reads = None;
    changed.lob_read_aheads = None;
    assert!(changed.is_valid());
    changed.issues.push(PlanPageServerIoIssue::DuplicateThread);
    assert!(!changed.is_valid());
}
