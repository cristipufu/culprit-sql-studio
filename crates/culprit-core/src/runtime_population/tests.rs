use super::*;

fn catalog() -> RuntimePopulationCatalog {
    RuntimePopulationCatalog {
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
        source: "unpartitioned-native-statistics".into(),
        partitioned: false,
        requires_selection: false,
        populations: Vec::new(),
    };
    assert!(source.is_valid());
    assert!(!source.contains(&NativeIdentity::new("0")), "an unpartitioned layout never invents a native group");
}
