use super::*;
use crate::{Engine, NativeRuntimePopulation, NativeRuntimePopulationCatalog};

fn history() -> QueryHistoryDescription {
    let populations = [
        NativeIdentity::new("same-native-id").qualified("generation", "first"),
        NativeIdentity::new("same-native-id").qualified("generation", "second"),
    ];
    QueryHistoryDescription {
        interval: TelemetryInterval {
            native_id: "source-interval".into(),
            from: "2026-11-01T00:00:00Z".into(),
            until: "2026-11-01T01:00:00Z".into(),
        },
        population_catalog: Some(NativeRuntimePopulationCatalog {
            membership: crate::RuntimePopulationMembership::NativeCatalog,
            source: "arbitrary-native-source".into(),
            partitioned: true,
            requires_selection: true,
            populations: populations
                .iter()
                .cloned()
                .map(|identity| NativeRuntimePopulation { identity, native_properties: BTreeMap::new() })
                .collect(),
        }),
        population_binding: QueryHistoryPopulationBinding::Dimensions {
            bindings: populations
                .into_iter()
                .zip([("east", "blue"), ("west", "green")])
                .map(|(population, (site, slot))| QueryHistoryPopulationBindingEntry {
                    population,
                    qualifiers: [("native-site".into(), site.into()), ("native-slot".into(), slot.into())].into(),
                })
                .collect(),
        },
        waits: None,
        dimensions: [
            ("native-site".into(), vec!["east".into(), "west".into()]),
            ("native-slot".into(), vec!["blue".into(), "green".into()]),
            ("native-outcome".into(), vec!["completed".into(), "failed".into()]),
        ]
        .into(),
        native_properties: BTreeMap::new(),
    }
}

fn qualifiers(site: &str, slot: &str) -> BTreeMap<String, String> {
    [
        ("native-site".into(), site.into()),
        ("native-slot".into(), slot.into()),
        ("native-outcome".into(), "completed".into()),
    ]
    .into()
}

#[test]
fn source_owned_tuples_resolve_complete_identities_without_engine_or_dimension_conventions() {
    let history = history();
    let first = &history.population_catalog.as_ref().unwrap().populations[0].identity;
    let second = &history.population_catalog.as_ref().unwrap().populations[1].identity;
    assert_ne!(first, second, "the native ID alone cannot select an incarnation");
    assert_eq!(
        history.execution_population(&qualifiers("east", "blue")).unwrap(),
        QueryExecutionPopulation::Population(first)
    );
    assert_eq!(
        history.execution_population(&qualifiers("west", "green")).unwrap(),
        QueryExecutionPopulation::Population(second)
    );
    let mut failed = qualifiers("west", "green");
    failed.insert("native-outcome".into(), "failed".into());
    assert_eq!(history.execution_population(&failed).unwrap(), QueryExecutionPopulation::Population(second));
}

#[test]
fn admissible_individual_values_do_not_authorize_an_undeclared_native_tuple() {
    let history = history();
    assert!(history.is_valid());
    let source = CollectionDescription {
        target: crate::DatabaseIdentity::server(Engine::Postgres, "fixture", "fixture.invalid", 5432, None).into(),
        partition: SourcePartition::Target,
        source: "arbitrary-native-source".into(),
        observation: SourceObservation::QueryHistory(history),
        continuity: SourceContinuity::Unverified,
    };
    let mut row = RuntimeMeasurement {
        key: NativeMeasurementKey {
            native: NativeIdentity { id: "native-row".into(), qualifiers: qualifiers("east", "blue") },
            references: StatementReferences::default(),
            cohort: None,
        },
        executions: "1".into(),
        metrics: vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, 1.0)],
        page_server_reads: None,
        first_execution_at: None,
        last_execution_at: None,
    };
    let capture = CaptureIdentity { observation_id: "native-capture".into(), revision: None };
    assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).is_ok());
    row.key.native.qualifiers = qualifiers("east", "green");
    assert!(!source.accepts_measurement(&row.key));
    assert!(ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row)).is_err());
}

#[test]
fn unknown_and_positively_unpartitioned_membership_remain_distinct() {
    let mut history = history();
    history.population_binding = QueryHistoryPopulationBinding::Unknown;
    assert_eq!(history.execution_population(&BTreeMap::new()).unwrap(), QueryExecutionPopulation::Unknown);
    history.population_catalog = None;
    assert!(history.is_valid());
    history.population_binding = QueryHistoryPopulationBinding::Unpartitioned;
    assert!(!history.is_valid(), "missing metadata cannot establish an unpartitioned workload");
    history.population_catalog = Some(NativeRuntimePopulationCatalog {
        membership: crate::RuntimePopulationMembership::NativeCatalog,
        source: "arbitrary-native-source".into(),
        partitioned: false,
        requires_selection: false,
        populations: vec![],
    });
    assert_eq!(history.execution_population(&BTreeMap::new()).unwrap(), QueryExecutionPopulation::Unpartitioned);
    history.population_binding = QueryHistoryPopulationBinding::Unknown;
    assert!(!history.is_valid(), "a positive unpartitioned declaration must not contradict its binding");
}

#[test]
fn overlapping_or_incomplete_binding_declarations_never_authorize_selection() {
    let original = history();
    for corruption in
        ["duplicate-tuple", "overlap", "missing-population", "different-identity", "undeclared-key", "undeclared-value"]
    {
        let mut history = original.clone();
        let QueryHistoryPopulationBinding::Dimensions { bindings } = &mut history.population_binding else { panic!() };
        match corruption {
            "duplicate-tuple" => bindings[1].qualifiers = bindings[0].qualifiers.clone(),
            "overlap" => {
                bindings[1].qualifiers.remove("native-site");
            }
            "missing-population" => {
                bindings.pop();
            }
            "different-identity" => {
                bindings[1].population.qualifiers.insert("generation".into(), "unobserved".into());
            }
            "undeclared-key" => {
                for binding in bindings {
                    binding.qualifiers.insert("invented".into(), "value".into());
                }
            }
            "undeclared-value" => {
                bindings[1].qualifiers.insert("native-site".into(), "unobserved".into());
            }
            _ => unreachable!(),
        }
        assert!(!history.is_valid(), "{corruption}");
        assert!(!SourceObservation::QueryHistory(history.clone()).covers_request(), "{corruption}");
        assert!(history.execution_population(&qualifiers("east", "blue")).is_err(), "{corruption}");
    }
}

#[test]
fn an_empty_partitioned_catalog_authorizes_no_native_row() {
    let mut history = history();
    history.population_catalog.as_mut().unwrap().populations.clear();
    history.population_catalog.as_mut().unwrap().requires_selection = false;
    history.population_binding = QueryHistoryPopulationBinding::Dimensions { bindings: vec![] };
    assert!(history.is_valid());
    assert!(history.execution_population(&qualifiers("east", "blue")).is_err());
}

#[test]
fn binding_metadata_has_a_total_byte_budget_even_when_each_native_identity_is_valid() {
    let mut history = history();
    let mut bindings = vec![];
    let mut populations = vec![];
    for index in 0..64 {
        let mut identity = NativeIdentity::new(format!("population-{index}"));
        for qualifier in 0..MAX_NATIVE_QUALIFIERS {
            identity = identity.qualified(format!("native-property-{qualifier}"), "x".repeat(512));
        }
        assert!(identity.is_valid());
        populations.push(NativeRuntimePopulation { identity: identity.clone(), native_properties: BTreeMap::new() });
        bindings.push(QueryHistoryPopulationBindingEntry {
            population: identity,
            qualifiers: [("slot".into(), index.to_string())].into(),
        });
    }
    history.dimensions = [("slot".into(), (0..64).map(|index| index.to_string()).collect())].into();
    history.population_catalog.as_mut().unwrap().populations = populations;
    assert!(history.population_catalog.as_ref().unwrap().is_valid());
    history.population_binding = QueryHistoryPopulationBinding::Dimensions { bindings };
    assert!(!history.is_valid());
}

#[test]
fn required_bindings_round_trip_and_are_not_invented_for_older_metadata() {
    let original = history();
    let mut value = serde_json::to_value(&original).unwrap();
    assert_eq!(serde_json::from_value::<QueryHistoryDescription>(value.clone()).unwrap(), original);
    value.as_object_mut().unwrap().remove("population_binding");
    assert!(serde_json::from_value::<QueryHistoryDescription>(value).is_err());
}

#[test]
fn observed_native_population_membership_cannot_be_transferred_to_another_history_interval() {
    let mut history = history();
    history.population_catalog.as_mut().unwrap().membership = crate::RuntimePopulationMembership::ObservedWindow {
        from: history.interval.from.clone(),
        until: history.interval.until.clone(),
    };
    assert!(history.is_valid());
    history.interval.until = "2026-11-01T02:00:00Z".into();
    assert!(!history.is_valid());
}
