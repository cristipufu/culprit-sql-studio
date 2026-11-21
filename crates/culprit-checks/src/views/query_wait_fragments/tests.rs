use super::*;
use culprit_core::Engine;

fn original(count: usize) -> CapturedQueryHistoryRead {
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Postgres, "original-db", "original.invalid", 5432, None)
            .into(),
        partition: SourcePartition::Target,
        source: "native-query-history".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            acquisition_owner: None,
            interval: TelemetryInterval {
                native_id: "1".into(),
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: Default::default(),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-query-wait-capture".into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-wait-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let read = CapturedReadRef::new(
        &source,
        capture.clone(),
        digest(b"original-manifest"),
        vec![CapturedStreamRows { stream: TelemetryStream::QueryWaits, rows: count as u32 }],
    )
    .unwrap();
    let records = (0..count)
        .map(|i| {
            let wait = WaitMeasurement {
                key: NativeMeasurementKey {
                    native: NativeIdentity::new(format!("fragment-{i}"))
                        .qualified("outcome", if i % 2 == 0 { "0" } else { "3" })
                        .qualified("replica", i.to_string()),
                    references: StatementReferences::default(),
                    cohort: None,
                },
                category: "native-category".into(),
                unit: RuntimeMetricUnit::Milliseconds,
                total: MeasurementValue::Integer("9007199254740993".into()),
                average: Some(1.25),
                minimum: None,
                maximum: None,
                last: Some(MeasurementValue::Integer(if i == 0 { "9007199254740993".into() } else { i.to_string() })),
                standard_deviation: Some(i as f64 + 0.75),
                category_description: Some("Native category".into()),
                outcome_description: Some(if i % 2 == 0 { "Regular".into() } else { "Aborted".into() }),
            };
            (ExportedRecord::new(&source, &capture, TelemetryRecord::QueryWaits(wait)).unwrap(), i as u32)
        })
        .collect::<Vec<_>>();
    CapturedQueryHistoryRead::from_exports(
        source,
        read,
        ReferenceCoverage::default(),
        ("2020-01-02T06:00:00Z".into(), "2020-01-02T06:00:01Z".into()),
        records,
        true,
    )
    .unwrap()
}
fn metric<'a>(entry: &'a QueryWaitFragmentEntry, key: &str) -> &'a str {
    entry.metrics.iter().find(|metric| metric.is(key)).unwrap().text.as_deref().unwrap()
}
#[test]
fn original_fragments_keep_wide_last_distinct_outcomes_replicas_and_native_deviation_without_pooling() {
    let read = original(2);
    let report = explain(&read).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(
        metric(&report.entries[0], "view.query_wait_fragments.metric.last"),
        t!("view.query_wait_fragments.quantity", value = format::digits("9007199254740993".into()), unit = "ms")
    );
    assert_eq!(
        metric(&report.entries[1], "view.query_wait_fragments.metric.last"),
        t!("view.query_wait_fragments.quantity", value = "1", unit = "ms")
    );
    assert_eq!(report.entries[0].native.native.qualifiers["outcome"], "0");
    assert_eq!(report.entries[1].native.native.qualifiers["outcome"], "3");
    assert_ne!(
        report.entries[0].native.native.qualifiers["replica"],
        report.entries[1].native.native.qualifiers["replica"]
    );
    assert_ne!(
        metric(&report.entries[0], "view.query_wait_fragments.metric.deviation"),
        metric(&report.entries[1], "view.query_wait_fragments.metric.deviation")
    );
    assert_eq!(report.description.as_deref(), Some(&read.description));
    assert!(report.limitations.contains(&t!("view.query_wait_fragments.limit.fragments")));
    assert!(report.limitations.contains(&t!("view.query_wait_fragments.limit.history")));
}
#[test]
fn native_null_partial_and_empty_are_distinct_from_missing_or_withdrawn_originals() {
    let mut read = original(2);
    let mut raw = read.rows[0].fragment.wait().unwrap().clone();
    raw.last = None;
    raw.standard_deviation = None;
    raw.category_description = None;
    raw.outcome_description = None;
    let row =
        ExportedRecord::new(&read.description, &read.read.capture, TelemetryRecord::QueryWaits(raw.clone())).unwrap();
    read.rows[0] = CapturedQueryHistoryRow {
        reference: read.read.bind(&read.description).unwrap().reference(&row, 0).unwrap(),
        fragment: QueryHistoryFragment::Waits(Box::new(raw)),
    };
    read.rows.truncate(1);
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.selected_rows, Some(2));
    assert_eq!(report.observed_rows, 1);
    assert_eq!(
        metric(&report.entries[0], "view.query_wait_fragments.metric.last"),
        t!("view.query_wait_fragments.unknown")
    );
    assert!(report.entries[0].outcome_description.is_none());
    let empty = explain(&original(0)).unwrap();
    assert_eq!(empty.selected_rows, Some(0));
    assert!(empty.entries.is_empty());
    assert!(empty.read.is_some());
    let missing = unavailable(&culprit_core::message!("view.query_wait_fragments.sharing_withheld"));
    assert_eq!(missing.selected_rows, None);
    assert_eq!(missing.source_state, SourceState::Unavailable);
    assert!(missing.read.is_none() && missing.description.is_none());
}
#[test]
fn source_clocks_original_positions_and_changed_fragment_bytes_cannot_be_repaired_by_display_labels() {
    let read = original(2);
    let mut changed = read.clone();
    changed.description.source = "different-source".into();
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    wait_mut(&mut changed.rows[0]).last = Some(MeasurementValue::Integer("7".into()));
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    wait_mut(&mut changed.rows[0]).outcome_description = Some("Regular".repeat(20));
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    changed.rows[1] = changed.rows[0].clone();
    assert!(explain(&changed).is_err());
    let mut changed = read.clone();
    changed.collected_until = "2020-01-02T05:00:00Z".into();
    assert!(explain(&changed).is_err());
    let mut changed = read;
    changed.read_complete = true;
    changed.rows.pop();
    assert!(explain(&changed).is_err());
}
#[test]
fn bounded_presentation_validates_the_whole_original_including_a_malformed_suffix() {
    let mut read = original(MAX_QUERY_WAIT_DETAIL_ENTRIES + 1);
    let report = explain(&read).unwrap();
    assert_eq!(report.observed_rows, 257);
    assert_eq!(report.entries.len(), 256);
    assert!(report.presentation_limited);
    assert!(report.limitations.contains(&t!("view.query_wait_fragments.limit.presentation", count = "256")));
    wait_mut(read.rows.last_mut().unwrap()).standard_deviation = Some(-1.0);
    assert!(explain(&read).is_err());
}

fn wait_mut(row: &mut CapturedQueryHistoryRow) -> &mut WaitMeasurement {
    let QueryHistoryFragment::Waits(wait) = &mut row.fragment else { panic!("wait fixture") };
    wait
}
