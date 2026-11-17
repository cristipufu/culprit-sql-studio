//! Controlled Query Store fragments exercise the production codecs; they are not native cost qualification.
use super::*;
use crate::analytical::evidence::EvidenceCapture;

pub struct PageServerReadCase {
    pub name: &'static str,
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
}

pub fn page_server_read_evidence_cases() -> Vec<PageServerReadCase> {
    let mut cases = Vec::new();
    for (name, availability) in [
        ("native_fragment", PageServerReadAvailability::Available),
        ("zero", PageServerReadAvailability::Available),
        ("native_null", PageServerReadAvailability::Available),
        ("not_applicable", PageServerReadAvailability::NotApplicable),
        ("profile_unknown", PageServerReadAvailability::Unknown),
        ("columns_unavailable", PageServerReadAvailability::Unavailable),
        ("separate_fragments", PageServerReadAvailability::Available),
        ("empty", PageServerReadAvailability::Available),
        ("single_fragment_with_definitions", PageServerReadAvailability::Available),
    ] {
        let source = source(availability);
        let statistics = if name == "zero" {
            PageServerReadStatistics {
                availability,
                average_pages: Some(0.0),
                last_pages: Some("0".into()),
                minimum_pages: Some("0".into()),
                maximum_pages: Some("0".into()),
                standard_deviation_pages: Some(0.0),
            }
        } else if availability == PageServerReadAvailability::Available && name != "native_null" {
            PageServerReadStatistics {
                availability,
                average_pages: Some(12.5),
                last_pages: Some("9007199254740993".into()),
                minimum_pages: Some("0".into()),
                maximum_pages: Some(i64::MAX.to_string()),
                standard_deviation_pages: Some(1.25),
            }
        } else {
            PageServerReadStatistics { availability, ..Default::default() }
        };
        let mut records = Vec::new();
        if name != "empty" {
            records.push(runtime(&source, "17", "4", "1", "0", statistics.clone()));
            if name == "single_fragment_with_definitions" {
                let TelemetryRecord::QueryRuntime(runtime) = &mut records[0] else { unreachable!() };
                runtime.executions = "1".into();
                runtime.page_server_reads = Some(Box::new(PageServerReadStatistics {
                    availability,
                    average_pages: Some(17.0),
                    last_pages: Some("17".into()),
                    minimum_pages: Some("17".into()),
                    maximum_pages: Some("17".into()),
                    standard_deviation_pages: Some(0.0),
                }));
                records.extend(definitions(&source));
            }
            if name == "separate_fragments" {
                // Persisted/in-memory fragments can have tied execution clocks. No arbitrary last wins.
                records.push(runtime(&source, "18", "4", "1", "0", statistics.clone()));
                records.push(runtime(&source, "19", "4", "1", "3", statistics.clone()));
                records.push(runtime(&source, "20", "4", "2", "0", statistics.clone()));
                records.push(runtime(&source, "21", "5", "1", "0", statistics));
            }
        }
        if name != "single_fragment_with_definitions" {
            for record in &mut records {
                if let TelemetryRecord::QueryRuntime(runtime) = record {
                    // Native IDs can be known while their definition bodies were not captured. Do not claim
                    // an exported definition ID unless that exact definition is included in this capture.
                    for reference in
                        [&mut runtime.key.references.query, &mut runtime.key.references.plan].into_iter().flatten()
                    {
                        reference.definition_id = None;
                    }
                }
            }
        }
        assert!(source.captured_range().is_ok());
        assert!(records.iter().all(TelemetryRecord::is_valid));
        cases.push(PageServerReadCase { name, source, records });
    }
    cases
}

fn source(availability: PageServerReadAvailability) -> CollectionDescription {
    let mut source = super::queries::source();
    let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
    database.engine = culprit_core::Engine::Mssql;
    database.incarnation = Some("query-store-logical-database".into());
    source.source = "mssql-query-history-fixture".into();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    history.interval.native_id = "1".into();
    history
        .native_properties
        .insert(PageServerReadAvailability::PROPERTY.into(), availability.native_property().into());
    history.population_catalog = Some(culprit_core::NativeRuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: source.source.clone(),
        partitioned: true,
        requires_selection: true,
        populations: ["1", "2"]
            .into_iter()
            .map(|group| culprit_core::NativeRuntimePopulation {
                identity: NativeIdentity::new(group),
                native_properties: Default::default(),
            })
            .collect(),
    });
    history.population_binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: ["1", "2"]
            .into_iter()
            .map(|group| QueryHistoryPopulationBindingEntry {
                population: NativeIdentity::new(group),
                qualifiers: [("replica_group_id".into(), group.into())].into(),
            })
            .collect(),
    };
    history.dimensions = [
        ("execution_type".into(), vec!["0".into(), "3".into(), "4".into()]),
        ("replica_group_id".into(), vec!["1".into(), "2".into()]),
    ]
    .into();
    source
}

fn native_query() -> NativeQueryIdentity {
    NativeQueryIdentity {
        query_id: "42".into(),
        qualifiers: [
            ("context_settings_id".into(), "5".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
        ]
        .into(),
    }
}
fn native_plan(plan: &str) -> NativePlanIdentity {
    NativePlanIdentity {
        plan_id: plan.into(),
        query: native_query(),
        qualifiers: [
            ("plan_hash".into(), "0011223344556677".into()),
            ("initial_compile_at".into(), "2020-01-01T05:00:00Z".into()),
        ]
        .into(),
    }
}
fn definitions(source: &CollectionDescription) -> Vec<TelemetryRecord> {
    let query = native_query();
    let query_id = query.definition_id(source).unwrap();
    let plan = native_plan("4");
    vec![
        TelemetryRecord::QueryDefinition(QueryDefinition {
            id: query_id.clone(),
            identity: query,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            context: Default::default(),
            context_available: false,
            variant_family: None,
            source_text_bytes: None,
            sql: DefinitionPayload::sql("SELECT 1 AS fixture_value", culprit_core::Engine::Mssql),
        }),
        TelemetryRecord::PlanDefinition(PlanDefinition {
            id: plan.definition_id(source).unwrap(),
            identity: plan,
            query_definition_id: Some(query_id),
            engine_version: None,
            native_properties: Default::default(),
            source_text_bytes: None,
            plan: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        }),
    ]
}
fn runtime(
    source: &CollectionDescription,
    id: &str,
    plan: &str,
    group: &str,
    outcome: &str,
    statistics: PageServerReadStatistics,
) -> TelemetryRecord {
    let query = native_query();
    let plan = native_plan(plan);
    let references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: query.native(),
            definition_id: Some(query.definition_id(source).unwrap()),
        }),
        plan: Some(NativeDefinitionReference {
            native: plan.native(),
            definition_id: Some(plan.definition_id(source).unwrap()),
        }),
    };
    let native = NativeIdentity::new(id)
        .qualified("plan_id", &plan.plan_id)
        .qualified("replica_group_id", group)
        .qualified("execution_type", outcome);
    let cohort = NativeIdentity::new("query-store-execution-population")
        .qualified("replica_group_id", group)
        .qualified("execution_type", outcome);
    TelemetryRecord::QueryRuntime(RuntimeMeasurement {
        key: NativeMeasurementKey { native, references, cohort: Some(cohort) },
        executions: "9007199254740993".into(),
        metrics: vec![RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Microseconds, 125.0)],
        page_server_reads: Some(Box::new(statistics)),
        first_execution_at: Some("2020-01-01T06:01:00Z".into()),
        last_execution_at: Some("2020-01-01T06:59:00Z".into()),
    })
}

pub fn assert_page_server_read_capture(
    capture: &EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_query_cohort_capture(capture, source, records);
    assert_eq!(
        capture.metrics().selected_rows as usize,
        records.iter().filter(|row| row.stream() == TelemetryStream::QueryRuntime).count()
    );
    assert_eq!(capture.metrics().recovery_objects, 0);
    for row in capture.records() {
        let TelemetryRecord::QueryRuntime(runtime) = &row.row().record else { panic!("runtime fragment") };
        assert!(runtime.page_server_reads_match_source(source));
        assert!(runtime.page_server_reads.as_ref().unwrap().is_valid());
    }
}
