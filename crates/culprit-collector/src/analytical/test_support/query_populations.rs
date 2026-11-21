//! Synthetic source declarations exercise archive membership without claiming a shipped PostgreSQL history reader.
use super::*;
use crate::analytical::evidence::EvidenceCapture;

const POPULATION_DIMENSION: &str = "native-routing-class";
const FIRST_VALUE: &str = "native-value-west";
const SECOND_VALUE: &str = "native-value-east";

/// One exact query/plan and one deliberately shared opaque cohort have different runtime and waits in two groups.
/// The source's tuple binding, rather than cohort spelling, native key names or role labels, separates the groups.
pub fn query_population_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut records) = super::queries::query_evidence();
    let populations = [
        (NativeIdentity::new("history-group-z").qualified("native-generation", "7"), FIRST_VALUE),
        (NativeIdentity::new("history-group-a").qualified("native-generation", "9"), SECOND_VALUE),
    ];
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("history fixture") };
    history.population_catalog = Some(culprit_core::NativeRuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: source.source.clone(),
        partitioned: true,
        requires_selection: true,
        populations: populations
            .iter()
            .map(|(identity, value)| culprit_core::NativeRuntimePopulation {
                identity: identity.clone(),
                native_properties: [("native-routing-value".into(), (*value).into())].into(),
            })
            .collect(),
    });
    history.population_binding = QueryHistoryPopulationBinding::Dimensions {
        bindings: populations
            .iter()
            .map(|(population, value)| QueryHistoryPopulationBindingEntry {
                population: population.clone(),
                qualifiers: [(POPULATION_DIMENSION.into(), (*value).into())].into(),
            })
            .collect(),
    };
    history.dimensions.insert(POPULATION_DIMENSION.into(), vec![FIRST_VALUE.into(), SECOND_VALUE.into()]);
    history.waits = Some(super::queries::wait_semantics(WaitCaptureState::Enabled));
    let references = records
        .iter()
        .find_map(|record| record.measurement_key().map(|key| key.references.clone()))
        .expect("exact native references");
    records.retain(|record| matches!(record, TelemetryRecord::QueryDefinition(_) | TelemetryRecord::PlanDefinition(_)));
    let cohort = NativeIdentity::new("same-opaque-native-cohort").qualified("native-outcome", "completed");
    for (value, count, duration, category, wait) in
        [(FIRST_VALUE, "2", 10.0, "native-lock", "7"), (SECOND_VALUE, "5", 100.0, "native-page-io", "90")]
    {
        let key = |id| NativeMeasurementKey {
            native: NativeIdentity::new(id).qualified(POPULATION_DIMENSION, value).qualified("execution_type", "0"),
            references: references.clone(),
            cohort: Some(cohort.clone()),
        };
        let mut metric = RuntimeMetric::average(RuntimeMetricKey::Duration, RuntimeMetricUnit::Milliseconds, duration);
        metric.minimum = Some(duration / 2.0);
        metric.maximum = Some(duration * 2.0);
        metric.standard_deviation = Some(1.0);
        records.push(TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: key("same-native-runtime-fragment-id"),
            executions: count.into(),
            metrics: vec![
                metric,
                RuntimeMetric::average(RuntimeMetricKey::CpuTime, RuntimeMetricUnit::Milliseconds, duration / 2.0),
            ],
            page_server_reads: None,
            first_execution_at: Some("2020-01-01T06:10:00Z".into()),
            last_execution_at: Some("2020-01-01T06:50:00Z".into()),
        }));
        records.push(TelemetryRecord::QueryWaits(WaitMeasurement {
            key: key("same-native-wait-fragment-id"),
            category: category.into(),
            unit: RuntimeMetricUnit::Milliseconds,
            total: MeasurementValue::Integer(wait.into()),
            average: None,
            minimum: None,
            maximum: None,
            last: None,
            standard_deviation: None,
            category_description: None,
            outcome_description: None,
        }));
    }
    assert!(source.query_history().unwrap().is_valid());
    (source, records)
}

/// Complete two-group history, a declared empty group, unknown binding with/without discovery, and explicit local history.
pub fn query_population_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let complete = query_population_evidence();
    let (source, mut records) = complete.clone();
    records.retain(|record| {
        record
            .measurement_key()
            .is_none_or(|key| key.native.qualifiers.get(POPULATION_DIMENSION).map(String::as_str) != Some(SECOND_VALUE))
    });
    let empty_group = (source, records);

    let (mut source, records) = complete.clone();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("history fixture") };
    history.population_binding = QueryHistoryPopulationBinding::Unknown;
    let unknown_with_catalog = (source, records);

    let (mut source, records) = unknown_with_catalog.clone();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("history fixture") };
    history.population_catalog = None;
    let unknown = (source, records);

    let (mut source, mut records) = empty_group.clone();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("history fixture") };
    history.population_catalog = Some(culprit_core::NativeRuntimePopulationCatalog {
        membership: culprit_core::RuntimePopulationMembership::NativeCatalog,
        source: source.source.clone(),
        partitioned: false,
        requires_selection: false,
        populations: Vec::new(),
    });
    history.population_binding = QueryHistoryPopulationBinding::Unpartitioned;
    history.dimensions.remove(POPULATION_DIMENSION);
    for record in &mut records {
        let key = match record {
            TelemetryRecord::QueryRuntime(row) => Some(&mut row.key),
            TelemetryRecord::QueryWaits(row) => Some(&mut row.key),
            _ => None,
        };
        if let Some(key) = key {
            key.native.qualifiers.remove(POPULATION_DIMENSION);
        }
    }
    vec![complete, empty_group, unknown_with_catalog, unknown, (source, records)]
}

pub fn assert_query_population_capture(
    capture: &EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_query_cohort_capture(capture, source, records);
    assert_eq!(capture.metrics().returned_rows as usize, capture.records().len());
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(capture.metrics().recovery_bytes, 0);
    let history = source.query_history().expect("native history description");
    assert!(history.is_valid());
}
