use super::*;

fn catalog() -> RuntimePopulationCatalog {
    RuntimePopulationCatalog {
        membership: crate::RuntimePopulationMembership::NativeCatalog,
        source: "native-statistics".into(),
        partitioned: true,
        requires_selection: true,
        populations: ["native-primary-group", "native-readable-group"]
            .into_iter()
            .map(|id| RuntimePopulation {
                identity: NativeIdentity::new(id),
                label: Message::from("Native source label"),
                native_properties: BTreeMap::new(),
            })
            .collect(),
    }
}

#[test]
fn exact_native_population_identity_is_independent_of_label_and_native_role_history() {
    let mut source = catalog();
    let selected = source.populations[1].identity.clone();
    source.populations[1].label = Message::from("A later localized label");
    source.populations[1].native_properties.insert("role_history".into(), "historical roles are not member IDs".into());
    assert!(source.contains(&selected));
    assert!(!source.contains(&selected.clone().qualified("unobserved", "role")));
    assert!(!source.contains(&NativeIdentity::new("A later localized label")));
    assert_eq!(
        serde_json::from_slice::<RuntimePopulationCatalog>(&serde_json::to_vec(&source).unwrap()).unwrap(),
        source
    );
    let native = source.native_facts();
    assert!(native.is_valid());
    assert_eq!(native.populations[1].identity, selected);
    assert!(!serde_json::to_string(&native).unwrap().contains("A later localized label"));
}

#[test]
fn sources_without_population_selection_reject_explicit_requests() {
    let mut options = crate::SnapshotOptions::default();
    assert!(options.require_default_workload_population().is_ok());
    options.workload_population = Some(NativeIdentity::new("17"));
    assert_eq!(options.require_default_workload_population().unwrap_err().code, crate::ErrorCode::Unsupported);
}

#[test]
fn observed_window_membership_is_positive_bounded_and_never_complete() {
    let mut observed = catalog();
    observed.membership = RuntimePopulationMembership::ObservedWindow {
        from: "2026-11-17T06:00:00Z".into(),
        until: "2026-11-17T07:00:00Z".into(),
    };
    assert!(observed.is_valid());
    assert!(observed.contains(&observed.populations[0].identity));
    assert!(!observed.has_complete_membership());
    assert!(!observed.native_facts().has_complete_membership());
    assert!(!observed.contains(&NativeIdentity::new("an-unreturned-group")));
    assert!(observed.membership.matches_window(Some("2026-11-17T06:00:00Z"), Some("2026-11-17T07:00:00Z")));
    assert!(!observed.membership.matches_window(Some("2026-11-16T06:00:00Z"), Some("2026-11-16T07:00:00Z")));
    observed.populations.clear();
    observed.requires_selection = false;
    assert!(!observed.is_valid(), "no observed group is not healthy empty or complete membership");
}

#[test]
fn duplicate_unknown_and_oversized_population_metadata_cannot_authorize_a_selection() {
    let original = catalog();
    for invalid in ["duplicate", "empty identity", "metadata budget", "unpartitioned alternatives"] {
        let mut source = original.clone();
        match invalid {
            "duplicate" => source.populations.push(source.populations[0].clone()),
            "empty identity" => source.populations[0].identity.id.clear(),
            "metadata budget" => {
                source.populations[0].native_properties.insert("native".into(), "x".repeat(16 * 1024 + 1));
            }
            _ => source.partitioned = false,
        }
        assert!(!source.is_valid(), "{invalid}");
        assert!(!source.contains(&original.populations[0].identity), "{invalid}");
    }
    let source = RuntimePopulationCatalog {
        membership: crate::RuntimePopulationMembership::NativeCatalog,
        source: "unpartitioned-native-statistics".into(),
        partitioned: false,
        requires_selection: false,
        populations: Vec::new(),
    };
    assert!(source.is_valid());
    assert!(!source.contains(&NativeIdentity::new("0")), "an unpartitioned layout never invents a native group");
}

#[test]
fn observed_window_reuses_strict_native_instants_and_retains_original_precision_and_offsets() {
    for (from, until) in [
        ("2026-11-17T06:00:00.0000001Z", "2026-11-17T06:00:00.0000002Z"),
        ("2026-11-17T06:00:00.000000001Z", "2026-11-17T06:00:00.000000002Z"),
        ("2026-11-17T08:00:00+02:00", "2026-11-17T06:00:00.000000001Z"),
        ("2026-11-17T06:00:00Z", "2026-11-17T06:00:01Z"),
    ] {
        let membership = RuntimePopulationMembership::ObservedWindow { from: from.into(), until: until.into() };
        assert!(membership.is_valid(), "{from}..{until}");
        assert!(membership.matches_window(Some(from), Some(until)));
        let restored: RuntimePopulationMembership =
            serde_json::from_value(serde_json::to_value(&membership).unwrap()).unwrap();
        assert_eq!(restored, membership, "validation never normalizes the original source bounds");
    }
    for (from, until) in [
        ("2026-11-17T06:00:00.0000000001Z", "2026-11-17T06:00:01Z"),
        ("2026-11-17T06:00:00Z", "2026-11-17T06:00:00.1000000001Z"),
        ("2026-11-17T08:00:00+02:00", "2026-11-17T06:00:00Z"),
        ("2026-11-17T08:00:00+01:00", "2026-11-17T06:00:00Z"),
    ] {
        let membership = RuntimePopulationMembership::ObservedWindow { from: from.into(), until: until.into() };
        assert!(!membership.is_valid(), "{from}..{until}");
        assert!(!membership.matches_window(Some(from), Some(until)));
    }
}
