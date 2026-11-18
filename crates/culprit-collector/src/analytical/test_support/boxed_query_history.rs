//! Controlled boxed native history is sealed through the real Outbox/warehouse corpus helpers.
//! No copied database GUID, fake replica role, or production cost qualification is supplied here.
use super::*;

pub fn boxed_query_history_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut case = super::page_server_reads::page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    case.source.target = culprit_core::DatabaseIdentity::server(
        culprit_core::Engine::Mssql,
        "owned-history",
        "native-instance.invalid",
        1433,
        None,
    )
    .into();
    case.source.source = culprit_core::source_id::QUERY_STORE.into();
    let owner = NativeReadContext {
        target: case.source.target.clone(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-machine").qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new("native-machine\\NATIVE").qualified("kind", "sql-server-instance"),
            }),
            epoch: Some(digest(b"controlled native recovery and process epoch")),
        },
        observed_from: "2020-01-01T08:00:00.123456789Z".into(),
        observed_until: "2020-01-01T08:00:00.123456789Z".into(),
        database_writable: Some(false),
    };
    let SourceObservation::QueryHistory(history) = &mut case.source.observation else { unreachable!() };
    history.acquisition_owner = Some(Box::new(owner));
    history.population_catalog = None;
    history.population_binding = QueryHistoryPopulationBinding::Unknown;
    history.dimensions.remove("replica_group_id");
    history.native_properties.insert(
        PageServerReadAvailability::PROPERTY.into(),
        PageServerReadAvailability::NotApplicable.native_property().into(),
    );
    history.native_properties.insert("runtime_replica_dimension".into(), "true".into());
    let query_id = case
        .records
        .iter()
        .find_map(|row| match row {
            TelemetryRecord::QueryDefinition(row) => Some(row.identity.definition_id(&case.source).unwrap()),
            _ => None,
        })
        .unwrap();
    let plan_id = case
        .records
        .iter()
        .find_map(|row| match row {
            TelemetryRecord::PlanDefinition(row) => Some(row.identity.definition_id(&case.source).unwrap()),
            _ => None,
        })
        .unwrap();
    // Every native definition/reference is constructed under this original logical namespace before sealing.
    for record in &mut case.records {
        match record {
            TelemetryRecord::QueryDefinition(row) => row.id = row.identity.definition_id(&case.source).unwrap(),
            TelemetryRecord::PlanDefinition(row) => {
                row.id = row.identity.definition_id(&case.source).unwrap();
                row.query_definition_id = Some(row.identity.query.definition_id(&case.source).unwrap());
            }
            TelemetryRecord::QueryRuntime(row) => {
                row.key.references.query.as_mut().unwrap().definition_id = Some(query_id.clone());
                row.key.references.plan.as_mut().unwrap().definition_id = Some(plan_id.clone());
                row.executions = "9007199254740993".into();
                row.page_server_reads = Some(Box::new(PageServerReadStatistics {
                    availability: PageServerReadAvailability::NotApplicable,
                    ..Default::default()
                }));
            }
            _ => unreachable!(),
        }
    }
    let mut completed = case.source.query_history().unwrap().clone();
    completed.acquisition_owner.as_mut().unwrap().observed_until = "2020-01-01T08:00:01.123456789Z".into();
    completed.population_catalog = Some(culprit_core::NativeRuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::ObservedWindow {
            from: completed.interval.from.clone(),
            until: completed.interval.until.clone(),
        },
        source: culprit_core::source_id::QUERY_STORE.into(),
        partitioned: true,
        requires_selection: false,
        populations: vec![culprit_core::NativeRuntimePopulation {
            identity: NativeIdentity::new("1"),
            native_properties: [("replica_group_id".into(), "1".into())].into(),
        }],
    });
    completed.population_binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: vec![QueryHistoryPopulationBindingEntry {
            population: NativeIdentity::new("1"),
            qualifiers: [("replica_group_id".into(), "1".into())].into(),
        }],
    };
    completed.dimensions.insert("replica_group_id".into(), vec!["1".into()]);
    let source = case.source.completed(SourceCompletion::QueryHistory(Box::new(completed))).unwrap().unwrap();
    let capture = CaptureIdentity { observation_id: "controlled-boxed-original".into(), revision: None };
    for row in &case.records {
        let original = ExportedRecord::new(&case.source, &capture, row.clone()).unwrap();
        assert_eq!(ExportedRecord::new(&source, &capture, original.record.clone()).unwrap(), original);
    }
    (source, case.records)
}
