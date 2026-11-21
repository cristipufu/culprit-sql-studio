use super::*;
fn original() -> CapturedQueryHistoryRead {
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "original",
            "original.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        source: "original-query-store".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            acquisition_owner: None,
            interval: TelemetryInterval {
                native_id: "1".into(),
                from: "2020-01-01T00:00:00Z".into(),
                until: "2020-01-01T01:00:00Z".into(),
            },
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: [(
                PageServerReadAvailability::PROPERTY.into(),
                PageServerReadAvailability::Available.native_property().into(),
            )]
            .into(),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-runtime".into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let read = CapturedReadRef::new(
        &source,
        capture.clone(),
        digest(b"original-manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: 2 }],
    )
    .unwrap();
    let rows = (0..2)
        .map(|i| {
            let metric = RuntimeMetric {
                key: RuntimeMetricKey::Duration,
                unit: RuntimeMetricUnit::Microseconds,
                average: if i == 0 { None } else { Some(1.25) },
                last: Some(MeasurementValue::Integer((9007199254740993u64 + i).to_string())),
                minimum: Some(MeasurementValue::Integer("0".into())),
                maximum: Some(MeasurementValue::Integer(i64::MAX.to_string())),
                standard_deviation: Some(i as f64 + 0.75),
            };
            let runtime = RuntimeMeasurement {
                key: NativeMeasurementKey {
                    native: NativeIdentity::new(format!("fragment-{i}"))
                        .qualified("original-outcome", i.to_string())
                        .qualified("original-replica", i.to_string()),
                    references: StatementReferences::default(),
                    cohort: None,
                },
                executions: (i + 2).to_string(),
                metrics: vec![
                    metric,
                    RuntimeMetric::average(RuntimeMetricKey::PhysicalReadOperations, RuntimeMetricUnit::Count, 1.25),
                ],
                page_server_reads: Some(Box::new(PageServerReadStatistics {
                    availability: PageServerReadAvailability::Available,
                    average_pages: None,
                    last_pages: Some("9007199254740993".into()),
                    minimum_pages: Some("0".into()),
                    maximum_pages: Some(i64::MAX.to_string()),
                    standard_deviation_pages: Some(0.75),
                })),
                first_execution_at: Some("2020-01-01T00:01:00Z".into()),
                last_execution_at: Some("2020-01-01T00:59:00Z".into()),
            };
            (ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(runtime)).unwrap(), i as u32)
        })
        .collect::<Vec<_>>();
    CapturedQueryHistoryRead::from_exports(
        source,
        read,
        ReferenceCoverage::default(),
        ("2020-01-02T00:00:00Z".into(), "2020-01-02T00:00:01Z".into()),
        rows,
        true,
    )
    .unwrap()
}
#[test]
fn independent_native_fragments_keep_exact_counts_unknown_mean_and_distinct_endpoints_without_representative_last() {
    let read = original();
    let report = explain(&read).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].executions, "2");
    assert_eq!(report.entries[1].executions, "3");
    assert_eq!(
        report.entries[0].metrics[0].statistics[0].text.as_deref(),
        Some(t!("view.runtime_fragments.unknown").as_str())
    );
    assert_eq!(
        report.entries[0].metrics[0].statistics[1].text.as_deref(),
        Some(t!("view.runtime_fragments.quantity", value = "9007199254740993", unit = "µs").as_str())
    );
    assert_ne!(report.entries[0].metrics[0].statistics[1].text, report.entries[1].metrics[0].statistics[1].text);
    assert_ne!(report.entries[0].native, report.entries[1].native);
    assert_eq!(report.description.as_deref(), Some(&read.description));
    assert!(report.limitations.contains(&t!("view.runtime_fragments.limit.fragments")));
    let original = read.rows[0].fragment.runtime().unwrap().page_server_reads.as_ref().unwrap();
    assert_eq!(report.entries[0].page_server_reads.as_ref(), Some(original));
    let pages = report.entries[0].page_server_explanation.as_ref().unwrap();
    assert_eq!(pages.metrics.len(), 5);
    assert_eq!(pages.metrics[0].text.as_deref(), Some(t!("view.page_server_reads.unknown").as_str()));
    assert_eq!(pages.metrics[1].text.as_deref(), Some(format::digits("9007199254740993".into()).as_str()));
    assert_eq!(pages.metrics[4].text.as_deref(), Some(format::digits("0.75".into()).as_str()));
}
#[test]
fn missing_partial_and_withdrawn_originals_have_independent_unknown_counts_and_no_native_replacement() {
    let mut read = original();
    read.rows.pop();
    read.read_complete = false;
    let partial = explain(&read).unwrap();
    assert_eq!(partial.selected_rows, Some(2));
    assert_eq!(partial.observed_rows, 1);
    assert_eq!(partial.source_state, SourceState::Partial);
    read.read_complete = true;
    assert!(explain(&read).is_err());
    let missing = unavailable(&culprit_core::message!("view.runtime_fragments.sharing_withheld"));
    assert_eq!(missing.selected_rows, None);
    assert!(missing.entries.is_empty() && missing.read.is_none() && missing.description.is_none());
}
#[test]
fn changed_original_bytes_source_clock_and_wrong_family_are_refused_before_rendering() {
    let read = original();
    let mut changed = read.clone();
    changed.description.source = "other".into();
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    changed.collected_until = "2019-01-01T00:00:00Z".into();
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    let QueryHistoryFragment::Runtime(runtime) = &mut changed.rows[1].fragment else { panic!() };
    runtime.metrics[0].last = Some(MeasurementValue::Integer("1".into()));
    assert!(explain(&changed).is_err());
    let mut changed = read;
    changed.read.streams[0].stream = TelemetryStream::QueryWaits;
    assert!(explain(&changed).is_err());
}
