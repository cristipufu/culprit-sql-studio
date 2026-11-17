use super::*;

fn read(input: &Evidence) -> CachedRuntimeRead {
    let mut read = input.runtime_read();
    let positions = input
        .records
        .iter()
        .zip(&input.references)
        .map(|(record, reference)| {
            let exported = ExportedRecord::new(&input.source, &reference.capture, record.clone()).unwrap();
            CachedRuntimeCapturedPosition::from_export(&exported, reference.row_ordinal).unwrap()
        })
        .collect();
    read.origin =
        CachedRuntimeReadOrigin::captured(&input.source, input.references[0].capture_ref(), positions).unwrap();
    assert!(read.is_valid());
    read
}

#[test]
fn one_shared_explanation_preserves_captured_receipts_instead_of_relabelling_local_input_native() {
    let input = Evidence::runtime(Some("9007199254740993"));
    let view = explain_read(&read(&input)).unwrap();
    assert_eq!(view.entries[0].observation.captured(), Some(&input.references[0]));
    assert!(view.read_complete);
    assert_eq!((view.native_rows, view.observed_rows), (1, 1));
    assert_eq!(view.entries[0].metrics[0].text.as_deref(), Some("9007199254740993"));
    let archived = fixture::report(&input, None);
    assert_eq!(
        serde_json::to_value(&view.entries[0].metrics).unwrap(),
        serde_json::to_value(&archived.entries[0].metrics).unwrap()
    );
    assert_eq!(view.entries[0].explanation, archived.entries[0].explanation);
}

#[test]
fn an_omitted_body_never_loses_original_captured_population_or_its_partial_qualification() {
    let input = Evidence::runtime(Some("17"));
    let mut missing = read(&input);
    missing.observations.clear();
    let CachedRuntimeReadOrigin::Captured { positions, .. } = &mut missing.origin else { panic!() };
    positions.clear();
    missing.read_complete = false;
    let view = explain_read(&missing).unwrap();
    assert_eq!((view.native_rows, view.observed_rows), (1, 0));
    assert!(!view.read_complete);
    assert_eq!(view.sources[0].state, SourceState::Partial);
    assert!(view.entries.is_empty());
    assert!(view.limitations.contains(&t!("view.completed_spills.limit.partial")));
}

#[test]
fn canonical_changed_bytes_and_foreign_source_namespace_cannot_reuse_a_captured_origin() {
    let input = Evidence::runtime(Some("17"));
    for changed_scope in [false, true] {
        let mut changed = read(&input);
        if changed_scope {
            let CollectionTarget::Database(database) = &mut changed.description.target else { panic!() };
            database.incarnation = Some("another-database-incarnation".into());
        } else {
            let OperationalFact::MssqlCachedRuntime(point) = &mut changed.observations[0].fact else { panic!() };
            point.counters.get_mut(&CachedRuntimeMetric::SpilledPages).unwrap().total = "18".into();
        }
        assert!(explain_read(&changed).is_err());
    }
}

#[test]
fn a_valid_independent_captured_read_cannot_borrow_an_enclosing_scans_known_namespace() {
    let input = Evidence::runtime(Some("17"));
    let CollectionTarget::Database(database) = &input.source.target else { panic!() };
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.server.engine = database.engine;
    snapshot.server.database = database.database.clone();
    snapshot.identity = Some(database.clone());
    snapshot.operations.cached_runtime = Some(Box::new(read(&input)));
    assert!(explain_snapshot(&snapshot).unwrap().is_some());

    let mut foreign = Evidence::runtime(Some("17"));
    let CollectionTarget::Database(database) = &mut foreign.source.target else { panic!() };
    database.database = "independently-captured-other-db".into();
    database.incarnation = Some("independently-captured-other-generation".into());
    foreign.refresh();
    let foreign = read(&foreign);
    assert!(foreign.is_valid());
    assert!(explain_read(&foreign).is_ok(), "independent analysis remains valid in its original namespace");
    snapshot.operations.cached_runtime = Some(Box::new(foreign));
    assert!(explain_snapshot(&snapshot).is_err());

    snapshot.operations.cached_runtime = Some(Box::new(read(&input)));
    snapshot.identity = None;
    assert!(explain_snapshot(&snapshot).is_err(), "a captured population cannot borrow an unknown scan identity");
    let unavailable = crate::analyze(snapshot.clone());
    assert!(unavailable.completed_spills.is_none());
    assert!(
        unavailable
            .snapshot
            .sources
            .iter()
            .any(|source| { source.source == CACHED_RUNTIME_READ_SOURCE && source.state == SourceState::Unavailable })
    );
    snapshot.operations.cached_runtime = Some(Box::new(input.runtime_read()));
    let independent = explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(independent.entries[0].observation.captured().is_none());
    assert!(independent.limitations.contains(&t!("view.observations.limit.namespace")));
}
