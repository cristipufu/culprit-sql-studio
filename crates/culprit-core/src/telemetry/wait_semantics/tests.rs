use super::*;
use std::collections::BTreeSet;

fn semantics() -> QueryWaitSemantics {
    QueryWaitSemantics {
        capture: WaitCaptureState::Enabled,
        coverage: WaitCaptureCoverage::Qualified,
        categories: [
            (
                "native-lock-id".into(),
                NativeWaitMeaning {
                    native_name: "Native Lock".into(),
                    disposition: WaitDisposition::Analyze(WaitClass::Lock),
                },
            ),
            (
                "native-idle-id".into(),
                NativeWaitMeaning { native_name: "Native Idle".into(), disposition: WaitDisposition::Ignore },
            ),
            (
                "native-unknown-id".into(),
                NativeWaitMeaning { native_name: "Native Unknown".into(), disposition: WaitDisposition::Unknown },
            ),
        ]
        .into(),
    }
}

#[test]
fn typed_classes_use_the_existing_unique_analytical_keys() {
    let mut keys = BTreeSet::new();
    for class in WaitClass::ALL {
        assert!(keys.insert(class.key()));
        assert_eq!(crate::waits::category_key(class.category()), class.key());
        assert_eq!(serde_json::to_value(class).unwrap(), serde_json::json!(class.key()));
    }
    assert_eq!(keys.len(), 14);
}

#[test]
fn native_names_ids_ignored_and_unknown_meanings_roundtrip_without_inference() {
    let original = semantics();
    assert!(original.is_valid());
    let decoded: QueryWaitSemantics = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(decoded.categories["native-lock-id"].disposition, WaitDisposition::Analyze(WaitClass::Lock));
    assert_eq!(decoded.categories["native-idle-id"].disposition, WaitDisposition::Ignore);
    assert_eq!(decoded.categories["native-unknown-id"].disposition, WaitDisposition::Unknown);
    assert!(!decoded.categories.contains_key("unrecognized-future-id"));
    assert_eq!(decoded.categories["native-lock-id"].native_name, "Native Lock");
}

#[test]
fn enabled_capture_requires_a_dictionary_but_never_establishes_read_completeness() {
    let mut evidence = QueryWaitSemantics {
        capture: WaitCaptureState::Enabled,
        coverage: WaitCaptureCoverage::Unverified,
        categories: BTreeMap::new(),
    };
    assert!(!evidence.is_valid());
    for capture in [WaitCaptureState::Disabled, WaitCaptureState::Unavailable] {
        evidence.capture = capture;
        assert!(evidence.is_valid());
    }
    let mut source = QueryHistoryDescription {
        interval: TelemetryInterval {
            native_id: "interval".into(),
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        waits: None,
        dimensions: Default::default(),
        native_properties: Default::default(),
    };
    assert!(source.is_valid());
    source.waits = Some(semantics());
    assert!(source.is_valid());
    source.waits.as_mut().unwrap().categories.clear();
    assert!(!source.is_valid());
}

#[test]
fn native_dictionary_entry_limit_is_enforced_before_excess_map_entries_are_accepted() {
    let mut evidence = semantics();
    let meaning = evidence.categories["native-lock-id"].clone();
    evidence.categories =
        (0..MAX_NATIVE_WAIT_CATEGORIES).map(|index| (format!("native-{index}"), meaning.clone())).collect();
    assert!(evidence.is_valid());
    assert!(serde_json::from_slice::<QueryWaitSemantics>(&serde_json::to_vec(&evidence).unwrap()).is_ok());
    evidence.categories.insert("one-too-many".into(), meaning);
    assert!(!evidence.is_valid());
    assert!(serde_json::from_slice::<QueryWaitSemantics>(&serde_json::to_vec(&evidence).unwrap()).is_err());
}

#[test]
fn invalid_native_ids_and_names_do_not_enter_the_typed_dictionary() {
    for id in [String::new(), "native\ncategory".into(), "x".repeat(MAX_NATIVE_WAIT_CATEGORY_BYTES + 1)] {
        let mut evidence = semantics();
        let meaning = evidence.categories.remove("native-lock-id").unwrap();
        evidence.categories.insert(id, meaning);
        assert!(!evidence.is_valid());
        assert!(serde_json::from_slice::<QueryWaitSemantics>(&serde_json::to_vec(&evidence).unwrap()).is_err());
    }
    for name in [String::new(), "native\0name".into(), "x".repeat(MAX_NATIVE_WAIT_NAME_BYTES + 1)] {
        let mut evidence = semantics();
        evidence.categories.get_mut("native-lock-id").unwrap().native_name = name;
        assert!(!evidence.is_valid());
        assert!(serde_json::from_slice::<QueryWaitSemantics>(&serde_json::to_vec(&evidence).unwrap()).is_err());
    }
}

#[test]
fn duplicate_native_ids_cannot_silently_replace_a_conflicting_meaning() {
    let duplicate = r#"{"capture":"enabled","coverage":"qualified","categories":{"native":{"native_name":"Native Lock","disposition":{"kind":"analyze","class":"lock"}},"native":{"native_name":"Native Memory","disposition":{"kind":"analyze","class":"memory"}}}}"#;
    assert!(serde_json::from_str::<QueryWaitSemantics>(duplicate).is_err());
    let unknown_class = r#"{"capture":"enabled","coverage":"qualified","categories":{"native":{"native_name":"Native Wait","disposition":{"kind":"analyze","class":"future-class"}}}}"#;
    assert!(serde_json::from_str::<QueryWaitSemantics>(unknown_class).is_err());
}

#[test]
fn current_mode_and_historical_interval_qualification_remain_independent_explicit_facts() {
    let mut evidence = semantics();
    for coverage in [WaitCaptureCoverage::Unverified, WaitCaptureCoverage::Qualified] {
        evidence.coverage = coverage;
        for capture in [WaitCaptureState::Enabled, WaitCaptureState::Disabled, WaitCaptureState::Unavailable] {
            evidence.capture = capture;
            assert!(evidence.is_valid());
            let decoded: QueryWaitSemantics = serde_json::from_slice(&serde_json::to_vec(&evidence).unwrap()).unwrap();
            assert_eq!(decoded.capture, capture);
            assert_eq!(decoded.coverage, coverage);
        }
    }
    let mut missing = serde_json::to_value(evidence).unwrap();
    missing.as_object_mut().unwrap().remove("coverage");
    assert!(serde_json::from_value::<QueryWaitSemantics>(missing).is_err());
}
