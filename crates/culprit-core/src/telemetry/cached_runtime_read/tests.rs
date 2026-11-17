use super::*;

fn read() -> CachedRuntimeRead {
    let mut context = crate::telemetry::native_read_context::tests::read_context();
    let runtime = CachedStatementRuntime {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2020-01-01T11:00:00.003".into(),
                plan_generation: "1".into(),
            },
            database_id: 7,
            query_hash: Some("0x0123456789ABCDEF".into()),
            origin: StatementOrigin::Unknown,
            monitoring_template_id: None,
            source_text_bytes: None,
            source_text_complete: false,
        },
        query_plan_hash: None,
        executions: Some("17".into()),
        counters: CachedRuntimeMetric::ALL
            .into_iter()
            .map(|metric| {
                (
                    metric,
                    CachedCounterDistribution {
                        total: "9007199254740993".into(),
                        last: "3".into(),
                        minimum: "0".into(),
                        maximum: "4".into(),
                    },
                )
            })
            .collect(),
        last_execution_without_offset: Some("2020-01-01T11:30:00.003".into()),
        reset: crate::ResetMarker::Unknown,
    };
    context.database_writable = None;
    let description = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: "logical-cache-reader".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::CachedRuntime,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: 1,
            sources: vec![StateSourceCoverage::observed("native-plan-cache".into(), 1, true)],
        }),
    };
    CachedRuntimeRead {
        read_id: cached_statement_read_id(&CaptureIdentity { observation_id: "1".repeat(32), revision: None }).unwrap(),
        read_complete: true,
        description,
        observations: vec![OperationalObservation {
            source: "native-plan-cache".into(),
            key: runtime.context.identity.key().unwrap(),
            fact: OperationalFact::MssqlCachedRuntime(Box::new(runtime)),
        }],
    }
}

#[test]
fn exact_cache_entry_values_and_native_context_survive_without_an_archive_receipt() {
    let read = read();
    assert!(read.is_valid());
    let decoded: CachedRuntimeRead = serde_json::from_slice(&serde_json::to_vec(&read).unwrap()).unwrap();
    assert_eq!(decoded, read);
    let OperationalFact::MssqlCachedRuntime(runtime) = &decoded.observations[0].fact else { panic!() };
    assert_eq!(runtime.counters[&CachedRuntimeMetric::SpilledPages].total, "9007199254740993");
    assert_ne!(
        decoded.description.source, decoded.observations[0].source,
        "logical and native namespaces are independent"
    );
    assert!(valid_digest(&read.read_key().unwrap()));
    assert!(!serde_json::to_string(&read).unwrap().contains("manifest_sha256"));
}

#[test]
fn missing_body_is_a_read_gap_and_never_rewrites_the_original_native_count() {
    let mut read = read();
    read.observations.clear();
    assert!(!read.is_valid());
    read.read_complete = false;
    assert!(read.is_valid());
    assert_eq!(read.description.observation.snapshot().unwrap().records, 1);
    let SourceObservation::Snapshot(snapshot) = &mut read.description.observation else { panic!() };
    snapshot.sources[0] =
        StateSourceCoverage::unavailable("native-plan-cache".into(), ErrorCode::PermissionDenied, None);
    snapshot.records = 0;
    assert!(read.is_valid(), "a genuinely unavailable native source retains its own empty read and failure");
}

#[test]
fn a_query_hash_never_collapses_distinct_entries_or_repairs_a_duplicate_native_key() {
    let mut read = read();
    let mut next = read.observations[0].clone();
    let OperationalFact::MssqlCachedRuntime(runtime) = &mut next.fact else { panic!() };
    runtime.context.identity.plan_generation = "2".into();
    next.key = runtime.context.identity.key().unwrap();
    read.observations.push(next);
    let SourceObservation::Snapshot(snapshot) = &mut read.description.observation else { panic!() };
    snapshot.records = 2;
    snapshot.sources[0] = StateSourceCoverage::observed("native-plan-cache".into(), 2, true);
    assert!(read.is_valid(), "two incarnations of the same hash remain two native points");
    read.observations[1] = read.observations[0].clone();
    assert!(!read.is_valid());
}

#[test]
fn foreign_facts_undeclared_sources_and_impossible_rosters_are_rejected() {
    for change in ["kind", "native-source", "key", "count", "scope", "clock"] {
        let mut read = read();
        match change {
            "kind" => {
                read.observations[0].fact = OperationalFact::SuspectPages {
                    count: "1".into(),
                    last_reported_time_without_offset: None,
                    reset: crate::ResetMarker::Unknown,
                }
            }
            "native-source" => read.observations[0].source = "undeclared-native-cache".into(),
            "key" => read.observations[0].key = digest(b"another-key"),
            "count" => {
                let SourceObservation::Snapshot(snapshot) = &mut read.description.observation else { panic!() };
                snapshot.records = 0;
            }
            "scope" => {
                let CollectionTarget::Database(database) = &mut read.description.target else { panic!() };
                database.engine = crate::Engine::Postgres;
                read.description.partition = SourcePartition::Target;
                assert!(
                    read.description.partition_is_consistent(),
                    "a valid foreign namespace cannot carry this native detail profile"
                );
            }
            "clock" => {
                let SourceObservation::Snapshot(snapshot) = &mut read.description.observation else { panic!() };
                snapshot.observed_until = "2019-01-01T00:00:00Z".into();
            }
            _ => unreachable!(),
        }
        if change == "kind" {
            assert!(read.observations[0].is_valid(), "a valid foreign fact still is not a cache-runtime sample");
        }
        assert!(!read.is_valid(), "{change}");
    }
}

#[test]
fn read_reference_changes_with_native_scope_and_clock_but_is_not_counter_continuity() {
    let original = read();
    let key = original.read_key().unwrap();
    let mut changed = original.clone();
    let OperationalFact::MssqlCachedRuntime(runtime) = &mut changed.observations[0].fact else { panic!() };
    runtime.counters.get_mut(&CachedRuntimeMetric::SpilledPages).unwrap().total = "9007199254740994".into();
    assert_eq!(
        changed.read_key().unwrap(),
        key,
        "row values are retained once and do not establish another read owner"
    );
    let SourceObservation::Snapshot(snapshot) = &mut changed.description.observation else { panic!() };
    snapshot.observed_until = "2020-01-01T12:00:02Z".into();
    assert_ne!(changed.read_key().unwrap(), key);
    let mut changed = original;
    let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.description.partition else { panic!() };
    *epoch = Some("new-native-process".into());
    assert_ne!(changed.read_key().unwrap(), key);
}

fn snapshot(read: CachedRuntimeRead) -> crate::Snapshot {
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.operations.cached_runtime = Some(Box::new(read));
    snapshot
}

#[test]
fn sharing_keeps_native_numbers_and_masks_reader_scope_and_unknown_labels_idempotently() {
    let mut read = read();
    let CollectionTarget::Database(database) = &mut read.description.target else { panic!() };
    database.location = crate::DatabaseLocation::Server {
        host: "private-cache-route.invalid".into(),
        port: 1433,
        instance: Some("private-cache-instance".into()),
    };
    database.incarnation = Some("private-cache-database-generation".into());
    let SourcePartition::InstanceLocal { member: Some(member), epoch } = &mut read.description.partition else {
        panic!()
    };
    member.namespace.id = "private-cache-machine".into();
    member.native.id = "private-cache-member".into();
    *epoch = Some("private-cache-process".into());
    read.description.source = "private-cache-reader-label".into();
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.sources[0].source = "private-cache-native-label".into();
    read.observations[0].source = "private-cache-native-label".into();
    let OperationalFact::MssqlCachedRuntime(runtime) = &mut read.observations[0].fact else { panic!() };
    runtime.reset = crate::ResetMarker::Generation("private-cache-counter-generation".into());
    assert!(read.is_valid());
    let original = snapshot(read);
    let first = crate::redact::redact_snapshot(&original);
    let shared = first.operations.cached_runtime.as_deref().unwrap();
    assert!(shared.is_valid());
    let text = serde_json::to_string(shared).unwrap();
    assert!(!text.contains("private-cache-"));
    assert!(text.contains("9007199254740993"));
    assert!(text.contains("2020-01-01T11:30:00.003"));
    assert_eq!(crate::redact::redact_snapshot(&first).operations.cached_runtime, first.operations.cached_runtime);
    assert!(
        serde_json::to_string(original.operations.cached_runtime.as_ref().unwrap())
            .unwrap()
            .contains("private-cache-route.invalid")
    );
}

#[test]
fn invalid_original_rows_are_withheld_before_masking_and_the_explicit_gap_survives_sharing_again() {
    for malformed_key in [false, true] {
        let mut read = read();
        if malformed_key {
            read.observations[0].key = digest(b"changed-original-key");
        } else {
            read.observations[0].fact = OperationalFact::SuspectPages {
                count: "1".into(),
                last_reported_time_without_offset: None,
                reset: crate::ResetMarker::Generation("private-foreign-cache-fact".into()),
            };
        }
        assert!(!read.is_valid());
        let original = snapshot(read);
        let first = crate::redact::redact_snapshot(&original);
        assert!(first.operations.cached_runtime.is_none());
        assert!(first.sources.iter().any(
            |source| source.source == CACHED_RUNTIME_READ_SOURCE && source.state == crate::SourceState::Unavailable
        ));
        assert!(!serde_json::to_string(&first).unwrap().contains("private-foreign-cache-fact"));
        let second = crate::redact::redact_snapshot(&first);
        let gap = second.sources.iter().find(|source| source.source == CACHED_RUNTIME_READ_SOURCE).unwrap();
        assert_eq!(gap.state, crate::SourceState::Unavailable);
        assert_eq!(gap.reason, Some(crate::t!("view.completed_spills.invalid")));
        assert!(second.operations.cached_runtime.is_none());
    }
}

#[test]
fn private_source_label_masking_collisions_withhold_the_carrier_instead_of_overwriting_a_point() {
    let mut read = read();
    let original_label = "private-cache-source-collision";
    let masked_label = format!("redacted-{}", digest(original_label.as_bytes()));
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.records = 2;
    header.sources = vec![
        StateSourceCoverage::observed(original_label.into(), 1, true),
        StateSourceCoverage::observed(masked_label.clone(), 1, true),
    ];
    read.observations[0].source = original_label.into();
    let mut second = read.observations[0].clone();
    second.source = masked_label;
    read.observations.push(second);
    assert!(read.is_valid(), "independent original native namespaces remain distinct before sharing");
    let original = snapshot(read);
    let shared = crate::redact::redact_snapshot(&original);
    assert!(shared.operations.cached_runtime.is_none());
    assert!(
        shared.sources.iter().any(
            |source| source.source == CACHED_RUNTIME_READ_SOURCE && source.state == crate::SourceState::Unavailable
        )
    );
    assert!(!serde_json::to_string(&shared).unwrap().contains(original_label));
    assert_eq!(original.operations.cached_runtime.as_ref().unwrap().observations.len(), 2);
}

#[test]
fn original_read_owner_is_independent_from_scope_body_completeness_and_counters() {
    let original = read();
    let mut other = original.clone();
    other.read_id =
        cached_statement_read_id(&CaptureIdentity { observation_id: "2".repeat(32), revision: None }).unwrap();
    assert!(other.is_valid());
    assert_eq!(other.description, original.description);
    assert_ne!(other.read_key().unwrap(), original.read_key().unwrap());
    let id = other.read_id.clone();
    other.read_complete = false;
    assert_eq!(other.read_id, id);
    assert_eq!(other.read_key().unwrap(), cached_statement_read_key(&id, &other.description).unwrap());
    for invalid in [String::new(), "A".repeat(64), "0".repeat(63), "0".repeat(65)] {
        other.read_id = invalid;
        assert!(!other.is_valid());
        assert!(other.read_key().is_err());
    }
}

#[test]
fn even_an_incomplete_body_cannot_declare_more_than_the_protocol_roster_limit() {
    let mut read = read();
    let limit = CollectionBudget::upper_limits().max_snapshot_rows;
    read.read_complete = false;
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.records = limit;
    header.sources[0] = StateSourceCoverage::observed("native-plan-cache".into(), limit, false);
    assert!(read.is_valid(), "a bounded missing body retains the native count without allocating fake rows");
    let SourceObservation::Snapshot(header) = &mut read.description.observation else { panic!() };
    header.records = limit + 1;
    header.sources[0] = StateSourceCoverage::observed("native-plan-cache".into(), limit + 1, false);
    assert!(!read.is_valid());
}
