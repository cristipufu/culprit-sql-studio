use super::*;
use culprit_core::{Engine, Snapshot, telemetry::*};

fn statistics() -> PageServerReadStatistics {
    PageServerReadStatistics {
        availability: PageServerReadAvailability::Available,
        average_pages: Some(12.5),
        last_pages: Some("9007199254740993".into()),
        minimum_pages: Some("0".into()),
        maximum_pages: Some(i64::MAX.to_string()),
        standard_deviation_pages: Some(1.25),
    }
}

struct Evidence {
    source: CollectionDescription,
    records: Vec<RuntimeMeasurement>,
    read: CapturedReadRef,
    reference_coverage: ReferenceCoverage,
    references: Vec<CapturedRecordReference>,
}
impl Evidence {
    fn new(count: usize) -> Self {
        let source = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, None).into(),
            partition: SourcePartition::Target,
            source: "original-query-store".into(),
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
                native_properties: [(
                    PageServerReadAvailability::PROPERTY.into(),
                    PageServerReadAvailability::Available.native_property().into(),
                )]
                .into(),
            }),
        };
        let query =
            NativeQueryIdentity { query_id: "42".into(), qualifiers: [("generation".into(), "1".into())].into() };
        let plan = NativePlanIdentity { plan_id: "4".into(), query: query.clone(), qualifiers: Default::default() };
        let references = StatementReferences {
            query: Some(NativeDefinitionReference {
                native: query.native(),
                definition_id: Some(query.definition_id(&source).unwrap()),
            }),
            plan: Some(NativeDefinitionReference {
                native: plan.native(),
                definition_id: Some(plan.definition_id(&source).unwrap()),
            }),
        };
        let records = (0..count)
            .map(|index| RuntimeMeasurement {
                key: NativeMeasurementKey {
                    native: NativeIdentity::new((index + 17).to_string())
                        .qualified("plan_id", "4")
                        .qualified("execution_type", "0")
                        .qualified("replica_group_id", "1"),
                    references: references.clone(),
                    cohort: None,
                },
                executions: "9007199254740993".into(),
                metrics: vec![],
                page_server_reads: Some(Box::new(statistics())),
                first_execution_at: Some("2020-01-01T06:01:00Z".into()),
                last_execution_at: Some("2020-01-01T06:59:00Z".into()),
            })
            .collect::<Vec<_>>();
        let capture = CaptureIdentity {
            observation_id: "a".repeat(32),
            revision: Some(CaptureRevision {
                outbox_id: "b".repeat(32),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let read = CapturedReadRef::new(
            &source,
            capture.clone(),
            digest(b"canonical-manifest"),
            vec![CapturedStreamRows { stream: TelemetryStream::QueryRuntime, rows: count as u32 }],
        )
        .unwrap();
        // Store the fixture's own canonical export before making its proof; incoming view data is never repaired.
        let records = records
            .into_iter()
            .map(|runtime| {
                let TelemetryRecord::QueryRuntime(runtime) =
                    ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(runtime)).unwrap().record
                else {
                    panic!()
                };
                runtime
            })
            .collect::<Vec<_>>();
        let reference_coverage = ReferenceCoverage {
            queries: DefinitionCoverage { required: count as u32, resolved: count as u32, unresolved: 0 },
            plans: DefinitionCoverage { required: count as u32, resolved: count as u32, unresolved: 0 },
            complete: true,
            integrity_complete: true,
            ..Default::default()
        };
        let references = records
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let exported =
                    ExportedRecord::new(&source, &capture, TelemetryRecord::QueryRuntime(row.clone())).unwrap();
                CapturedRecordReference::from_export(&read, &source, &exported, index as u32).unwrap()
            })
            .collect();
        Self { source, records, references, read, reference_coverage }
    }
    fn rows(&self) -> Vec<RuntimeEvidence<'_>> {
        self.records
            .iter()
            .zip(&self.references)
            .map(|(runtime, reference)| RuntimeEvidence { reference: reference.clone(), runtime })
            .collect()
    }
}

#[test]
fn native_float_and_bigint_statistics_are_context_not_current_cost_or_delay() {
    let evidence = Evidence::new(2);
    let report = explain_archive(PageServerReadEvidence {
        source: &evidence.source,
        read: &evidence.read,
        reference_coverage: &evidence.reference_coverage,
        rows: &evidence.rows(),
        selected_rows: 2,
        read_complete: true,
        definitions: None,
    })
    .unwrap();
    assert_eq!(report.entries.len(), 2, "tied clocks do not select one representative fragment");
    assert_eq!(report.entries[0].executions, "9007199254740993");
    let explanation = &report.entries[0].explanation;
    assert_eq!(
        explanation
            .metrics
            .iter()
            .find(|metric| metric.is("view.page_server_reads.metric.last"))
            .unwrap()
            .text
            .as_deref(),
        Some("9007199254740993")
    );
    assert_eq!(
        explanation
            .metrics
            .iter()
            .find(|metric| metric.is("view.page_server_reads.metric.maximum"))
            .unwrap()
            .text
            .as_deref(),
        Some(i64::MAX.to_string().as_str())
    );
    for qualification in [
        t!("view.page_server_reads.limit.lifetime"),
        t!("view.page_server_reads.limit.storage"),
        t!("view.page_server_reads.limit.origin"),
    ] {
        assert!(explanation.limitations.contains(&qualification));
    }
    let json = serde_json::to_value(&report).unwrap();
    assert!(
        json.get("findings").is_none() && json.get("latency_ms").is_none() && json.get("current_pressure").is_none()
    );
    let PageServerReadEntryIdentity::RuntimeFragment { reference, native } = &report.entries[0].identity else {
        panic!()
    };
    assert_eq!(reference.as_ref(), &evidence.references[0]);
    assert_eq!(native.as_ref(), &evidence.records[0].key);
}

#[test]
fn supported_native_null_zero_and_each_unavailable_profile_stay_distinct() {
    let null = explain_statistics(
        &PageServerReadStatistics { availability: PageServerReadAvailability::Available, ..Default::default() },
        None,
    )
    .unwrap();
    assert_eq!(null.metrics.len(), 5);
    assert!(
        null.metrics.iter().all(|metric| metric.text.as_deref() == Some(t!("view.page_server_reads.unknown").as_str()))
    );
    let zero = explain_statistics(
        &PageServerReadStatistics {
            availability: PageServerReadAvailability::Available,
            last_pages: Some("0".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert_ne!(zero.summary, null.summary);
    assert_eq!(
        zero.metrics.iter().find(|metric| metric.is("view.page_server_reads.metric.last")).unwrap().text.as_deref(),
        Some("0")
    );
    let summaries = [
        PageServerReadAvailability::NotApplicable,
        PageServerReadAvailability::Unavailable,
        PageServerReadAvailability::Unknown,
    ]
    .map(|availability| {
        let explanation =
            explain_statistics(&PageServerReadStatistics { availability, ..Default::default() }, None).unwrap();
        assert!(explanation.metrics.is_empty());
        explanation.summary
    });
    assert_eq!(summaries.iter().collect::<BTreeSet<_>>().len(), 3);
}

#[test]
fn canonical_receipts_reject_changed_counts_scope_dimensions_ordinal_and_noncanonical_clocks() {
    for change in 0..6 {
        let mut evidence = Evidence::new(1);
        match change {
            0 => evidence.records[0].page_server_reads.as_mut().unwrap().last_pages = Some("0".into()),
            1 => evidence.source.source = "different-native-source".into(),
            2 => {
                evidence.records[0].key.native.qualifiers.insert("replica_group_id".into(), "2".into());
            }
            3 => diagnostic_mut(&mut evidence.references[0]).row_ordinal = 1,
            4 => evidence.records[0].last_execution_at = Some("2020-01-01T06:59:00.0000000+00:00".into()),
            _ => {
                let CollectionTarget::Database(database) = &mut evidence.source.target else { panic!() };
                database.database = "another-db".into();
            }
        }
        assert!(
            explain_archive(PageServerReadEvidence {
                source: &evidence.source,
                read: &evidence.read,
                reference_coverage: &evidence.reference_coverage,
                rows: &evidence.rows(),
                selected_rows: 1,
                read_complete: true,
                definitions: None
            })
            .is_err(),
            "change {change}"
        );
    }
    let mut evidence = Evidence::new(2);
    diagnostic_mut(&mut evidence.references[1]).capture.observation_id = "c".repeat(32);
    assert!(
        explain_archive(PageServerReadEvidence {
            source: &evidence.source,
            read: &evidence.read,
            reference_coverage: &evidence.reference_coverage,
            rows: &evidence.rows(),
            selected_rows: 2,
            read_complete: true,
            definitions: None
        })
        .is_err()
    );
}

#[test]
fn selection_gaps_and_empty_native_selection_never_become_healthy_zero() {
    let evidence = Evidence::new(1);
    assert!(
        explain_archive(PageServerReadEvidence {
            source: &evidence.source,
            read: &evidence.read,
            reference_coverage: &evidence.reference_coverage,
            rows: &[],
            selected_rows: 1,
            read_complete: true,
            definitions: None
        })
        .is_err()
    );
    let missing = explain_archive(PageServerReadEvidence {
        source: &evidence.source,
        read: &evidence.read,
        reference_coverage: &evidence.reference_coverage,
        rows: &[],
        selected_rows: 1,
        read_complete: false,
        definitions: None,
    })
    .unwrap();
    assert_eq!(missing.source_state, Some(SourceState::Partial));
    assert!(missing.entries.is_empty());
    assert!(missing.limitations.contains(&t!("view.page_server_reads.limit.read")));
    let empty_evidence = Evidence::new(0);
    let empty = explain_archive(PageServerReadEvidence {
        source: &empty_evidence.source,
        read: &empty_evidence.read,
        reference_coverage: &empty_evidence.reference_coverage,
        rows: &[],
        selected_rows: 0,
        read_complete: true,
        definitions: None,
    })
    .unwrap();
    assert_eq!(empty.source_state, Some(SourceState::Empty));
    assert!(empty.entries.is_empty());
    assert!(empty.limitations.contains(&t!("view.page_server_reads.limit.population")));
    let mut unavailable_source = empty_evidence.source.clone();
    let SourceObservation::QueryHistory(history) = &mut unavailable_source.observation else { panic!() };
    history.native_properties.insert(
        PageServerReadAvailability::PROPERTY.into(),
        PageServerReadAvailability::Unavailable.native_property().into(),
    );
    let unavailable_read = CapturedReadRef::new(
        &unavailable_source,
        empty_evidence.read.capture.clone(),
        empty_evidence.read.manifest_sha256.clone(),
        empty_evidence.read.streams.clone(),
    )
    .unwrap();
    let unavailable = explain_archive(PageServerReadEvidence {
        source: &unavailable_source,
        read: &unavailable_read,
        reference_coverage: &empty_evidence.reference_coverage,
        rows: &[],
        selected_rows: 0,
        read_complete: true,
        definitions: None,
    })
    .unwrap();
    assert_eq!(unavailable.availability, Some(PageServerReadAvailability::Unavailable));
    assert!(unavailable.limitations.contains(&t!("view.page_server_reads.unavailable")));
}

fn workload() -> Workload {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut workload = snapshot.workload.unwrap();
    let mut query = workload.queries.remove(0);
    let native_query = NativeQueryIdentity { query_id: "42".into(), qualifiers: Default::default() };
    query.native_identity = Some(native_query.clone());
    query.plans.truncate(1);
    let plan = &mut query.plans[0];
    plan.native_identity =
        Some(NativePlanIdentity { plan_id: "4".into(), query: native_query, qualifiers: Default::default() });
    plan.id = "4".into();
    plan.page_server_reads = Some(Box::new(PageServerReadSummary::NativeFragment {
        native: Some(NativeIdentity::new("17").qualified("plan_id", "4").qualified("execution_type", "0")),
        executions: "9007199254740993".into(),
        statistics: Box::new(statistics()),
    }));
    workload.queries = vec![query];
    workload
}

#[test]
fn plan_summaries_keep_exact_counts_and_never_average_distinct_plans_or_outcomes() {
    let mut workload = workload();
    let mut next = workload.queries[0].plans[0].clone();
    next.id = "5".into();
    next.native_identity.as_mut().unwrap().plan_id = "5".into();
    let mut different = statistics();
    different.average_pages = Some(3.0);
    different.last_pages = None;
    different.standard_deviation_pages = None;
    next.page_server_reads = Some(Box::new(PageServerReadSummary::Derived {
        native_fragments: "2".into(),
        executions: "7".into(),
        execution_type: Some("0".into()),
        statistics: Box::new(different),
    }));
    workload.queries[0].plans.push(next);
    let report = explain_workload(&workload, None).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].executions, "9007199254740993");
    assert_eq!(report.entries[1].executions, "7");
    assert!(!report.read_complete, "missing source coverage is explicit");
    let derived = &report.entries[1].explanation;
    assert_eq!(
        derived.metrics.iter().find(|metric| metric.is("view.page_server_reads.metric.last")).unwrap().text.as_deref(),
        Some(t!("view.page_server_reads.unknown").as_str())
    );
    let PageServerReadSummary::Derived { execution_type, .. } =
        workload.queries[0].plans[1].page_server_reads.as_mut().unwrap().as_mut()
    else {
        panic!()
    };
    *execution_type = None;
    assert!(explain_workload(&workload, None).is_err(), "a mixed outcome average is refused");
    let PageServerReadSummary::Derived { statistics, .. } =
        workload.queries[0].plans[1].page_server_reads.as_mut().unwrap().as_mut()
    else {
        panic!()
    };
    statistics.average_pages = None;
    statistics.minimum_pages = None;
    statistics.maximum_pages = None;
    assert!(
        explain_workload(&workload, None).unwrap().entries[1]
            .explanation
            .limitations
            .contains(&t!("view.page_server_reads.limit.outcomes"))
    );
}

#[test]
fn bounded_presentation_still_validates_every_original_row_and_retains_full_counts() {
    let mut evidence = Evidence::new(MAX_DETAIL_ENTRIES + 1);
    let report = explain_archive(PageServerReadEvidence {
        source: &evidence.source,
        read: &evidence.read,
        reference_coverage: &evidence.reference_coverage,
        rows: &evidence.rows(),
        selected_rows: evidence.records.len() as u32,
        read_complete: true,
        definitions: None,
    })
    .unwrap();
    assert_eq!(report.entries.len(), MAX_DETAIL_ENTRIES);
    assert_eq!(report.observed_rows, MAX_DETAIL_ENTRIES as u32 + 1);
    assert!(report.presentation_limited && report.read_complete);
    evidence.records.last_mut().unwrap().page_server_reads.as_mut().unwrap().last_pages = Some("0".into());
    assert!(
        explain_archive(PageServerReadEvidence {
            source: &evidence.source,
            read: &evidence.read,
            reference_coverage: &evidence.reference_coverage,
            rows: &evidence.rows(),
            selected_rows: evidence.records.len() as u32,
            read_complete: true,
            definitions: None
        })
        .is_err(),
        "a hidden detail row is still authenticated"
    );
}

#[test]
fn display_plan_labels_do_not_replace_native_plan_identity_or_authorize_a_different_plan() {
    let mut workload = workload();
    workload.queries[0].plans[0].id = digest(b"archive definition display id");
    assert!(explain_workload(&workload, None).is_ok());
    let PageServerReadSummary::NativeFragment { native: Some(native), .. } =
        workload.queries[0].plans[0].page_server_reads.as_mut().unwrap().as_mut()
    else {
        panic!()
    };
    native.qualifiers.insert("plan_id".into(), "5".into());
    assert!(explain_workload(&workload, None).is_err());
}

fn diagnostic_mut(reference: &mut CapturedRecordReference) -> &mut DiagnosticEvidenceRef {
    let CapturedRecordReference::Diagnostic { reference } = reference else { panic!("resolved fixture") };
    reference
}

mod informational;
