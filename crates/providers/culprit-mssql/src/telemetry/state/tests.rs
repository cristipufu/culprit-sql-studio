use super::*;
use crate::{
    operations::native,
    resources::test_support::{FakeRead, latest_row, replace},
    value::{Rec, Val},
};
use culprit_core::SourceState;

fn identity() -> Rec {
    Rec::of(&[
        ("database_name", Val::Text("sample".into())),
        ("incarnation", Val::Text("logical-guid".into())),
        ("physical_incarnation", Val::Text("physical-guid".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
    ])
}
fn database_context() -> subject::Identity {
    let mut database =
        culprit_core::DatabaseIdentity::server(culprit_core::Engine::Mssql, "sample", "fixture.invalid", 1433, None);
    database.incarnation = Some("logical-guid".into());
    subject::Identity::Database { database, physical: "physical-guid".into() }
}
pub(super) fn clock(metadata: bool, at: &str) -> Rec {
    Rec::of(&[("observed_at", Val::Text(at.into())), ("metadata_visibility", Val::Int(i64::from(metadata)))])
}
pub(super) fn pressure(high: bool) -> Rec {
    replace(
        &latest_row("2020-01-01T06:01:30Z"),
        &[
            ("avg_instance_cpu_percent", Val::Float(if high { 90.0 } else { 10.0 })),
            ("max_worker_percent", Val::Float(10.0)),
            ("avg_data_io_percent", Val::Float(10.0)),
            ("avg_log_write_percent", Val::Float(10.0)),
        ],
    )
}
pub(super) fn space_row() -> Rec {
    Rec::of(&[
        ("native_id", Val::Int(1)),
        ("name", Val::Text("PRIMARY".into())),
        ("total", Val::Float(1000.0)),
        ("used", Val::Float(100.0)),
        ("max_bytes", Val::Null),
        ("can_grow", Val::Int(1)),
        ("reuse_wait", Val::Text("NOTHING".into())),
        ("users", Val::Float(50.0)),
        ("internals", Val::Null),
        ("versions", Val::Float(50.0)),
    ])
}
fn sequence() -> Rec {
    Rec::of(&[
        ("native_id", Val::Text("identity:123:1".into())),
        ("name", Val::Text("dbo.items.id".into())),
        ("table_name", Val::Text("dbo.items".into())),
        ("current_value", Val::Text("9007199254740993".into())),
        ("step", Val::Text("1".into())),
        ("min_value", Val::Text("1".into())),
        ("max_value", Val::Text("9223372036854775807".into())),
        ("cycles", Val::Int(0)),
    ])
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget { page_rows: 2, ..Default::default() },
    }
}
fn source(metadata: bool, fail_files: bool, final_identity: Rec) -> StateSource<FakeRead> {
    let mut replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock(metadata, "2020-01-01T06:02:00Z")]),
    ];
    for kind in native::Kind::ALL {
        if matches!(kind, native::Kind::SuspectPages | native::Kind::Temp)
            || (kind == native::Kind::Sequences && !metadata)
        {
            continue;
        }
        replies.push(Ok(vec![pressure(false)]));
        replies.push(if fail_files && kind == native::Kind::Filegroups {
            Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture permission denied")))
        } else {
            Ok(vec![if kind == native::Kind::Sequences { sequence() } else { space_row() }])
        });
    }
    replies.extend([Ok(vec![pressure(false)]), Ok(vec![crate::storage_io::tests::row()])]);
    for _ in crate::queues::KINDS {
        replies.extend([Ok(vec![pressure(false)]), Ok(vec![crate::queues::tests::row()])]);
    }
    replies.extend([Ok(vec![pressure(false)]), Ok(vec![crate::version_store::tests::row()])]);
    replies.extend([Ok(vec![pressure(false)]), Ok(vec![])]);
    replies.extend([
        Ok(vec![pressure(false)]),
        Ok(vec![crate::version_store::persistent::tests::layout(5, 12, 1, 1)]),
        Ok(vec![Rec::of(&[("adr", Val::Int(0))])]),
    ]);
    replies.extend([Ok(vec![clock(metadata, "2020-01-01T06:02:01Z")]), Ok(vec![final_identity])]);
    StateSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap()
}

#[tokio::test]
async fn missing_index_capture_preserves_partial_associations_and_catalog_visibility() {
    for (metadata, final_metadata, links) in
        [(true, true, true), (true, true, false), (true, false, true), (false, false, true)]
    {
        let mut replies = vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![identity()]),
            Ok(vec![clock(metadata, "2020-01-01T06:02:00Z")]),
        ];
        if metadata {
            replies.extend([Ok(vec![pressure(false)]), Ok(vec![crate::missing_indexes::tests::row()])]);
            replies.push(if links {
                Ok(Vec::new())
            } else {
                Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture")))
            });
        }
        replies.extend([Ok(vec![clock(final_metadata, "2020-01-01T06:02:01Z")]), Ok(vec![identity()])]);
        let request = CollectionRequest {
            subject: culprit_core::telemetry::CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::MissingIndexes },
            budget: Default::default(),
        };
        let mut source =
            StateSource::with_io(FakeRead::new(replies), &request, "fixture.invalid".into(), 1433).unwrap();
        let description = source.describe().await.unwrap();
        let coverage = &description.observation.snapshot().unwrap().sources[0];
        assert_eq!(coverage.source, crate::sources::MISSING_INDEXES.name);
        if metadata {
            assert_eq!(coverage.records, 1);
            assert_eq!(coverage.complete_scope, final_metadata && links);
            assert_eq!(
                coverage.state,
                if final_metadata && links { SourceState::Collected } else { SourceState::Partial }
            );
            let CollectionRead::Page { records, .. } =
                source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
            else {
                panic!("advice page")
            };
            let TelemetryRecord::Operational(observation) = &records[0] else { panic!("observation") };
            let OperationalFact::MssqlMissingIndex(index) = &observation.fact else { panic!("advice") };
            assert_eq!(index.query_hashes.is_some(), links);
            assert_eq!(observation.key, index.key().unwrap());
            assert!(observation.is_valid());
        } else {
            assert_eq!(coverage.state, SourceState::Unavailable);
            assert_eq!(coverage.records, 0);
            let CollectionRead::Page { records, .. } =
                source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
            else {
                panic!("empty page")
            };
            assert!(records.is_empty());
        }
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn bounded_operational_pages_retain_units_exact_sequence_values_and_unknown_parts() {
    let mut source = source(true, false, identity());
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 9);
    assert_eq!(
        snapshot.sources.iter().find(|source| source.source == crate::sources::SUSPECT_PAGES.name).unwrap().state,
        SourceState::NotApplicable
    );
    for shared in [
        crate::sources::TEMP_CAPACITY.name,
        crate::sources::PERFORMANCE_COUNTERS.name,
        crate::sources::PROCESSOR_TOPOLOGY.name,
        crate::sources::COMPILATION_GATEWAYS.name,
    ] {
        assert!(!snapshot.sources.iter().any(|source| source.source == shared));
    }
    let mut cursor = None;
    let mut records = vec![];
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("complete page")
        };
        assert!(page.len() <= 2);
        records.extend(page);
        if next_cursor.is_none() {
            break;
        }
        cursor = next_cursor;
    }
    assert_eq!(records.len(), 9);
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    for record in &records {
        let exported = ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "state-fixture".into(), revision: None },
            record.clone(),
        )
        .unwrap();
        assert!(exported.record_id.is_some());
    }
    let sequence = records
        .iter()
        .find_map(|record| match record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::Sequence(sequence), ..
            }) => Some(sequence),
            _ => None,
        })
        .expect("exact sequence evidence");
    assert_eq!(sequence.current, "9007199254740993");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn missing_permissions_and_restricted_metadata_are_never_reported_as_empty_sources() {
    let mut source = source(false, true, identity());
    let description = source.describe().await.unwrap();
    let sources = &description.observation.snapshot().unwrap().sources;
    for source in [crate::sources::FILE_CAPACITY.name, crate::sources::SEQUENCE_CAPACITY.name] {
        let source = sources.iter().find(|item| item.source == source).unwrap();
        assert_eq!(source.state, SourceState::Unavailable);
        assert_eq!(source.records, 0);
        assert_eq!(source.error_code, Some(ErrorCode::PermissionDenied));
        assert!(!source.complete_scope);
    }
    assert_eq!(sources[0].state, SourceState::Collected);
    assert_eq!(
        sources.iter().find(|source| source.source == crate::sources::LOG_CAPACITY.name).unwrap().state,
        SourceState::Collected
    );
}

#[tokio::test]
async fn source_pressure_stops_before_further_statistics_clocks_or_verification_queries() {
    for (sample, reason) in [
        (pressure(true), CollectionStop::Pressure),
        (replace(&pressure(false), &[("avg_cpu_percent", Val::Float(95.0))]), CollectionStop::Pressure),
        (replace(&pressure(false), &[("avg_cpu_percent", Val::Null)]), CollectionStop::PressureUnavailable),
    ] {
        let replies = vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![identity()]),
            Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
            Ok(vec![sample]),
        ];
        let mut source =
            StateSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), reason);
        assert!(source.description.is_none());
        assert!(source.finish().await.is_err());
        assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_read_only_replica_is_rejected_before_operational_reads_and_at_final_verification() {
    let readonly = replace(&identity(), &[("updateability", Val::Text("READ_ONLY".into()))]);
    let replies = vec![Ok(vec![Rec::of(&[("edition", Val::Int(5))])]), Ok(vec![readonly.clone()])];
    let mut initial = StateSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    assert_eq!(initial.describe().await.unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(initial.query_count(), 2);
    let mut changed = source(true, false, readonly);
    changed.describe().await.unwrap();
    let mut cursor = None;
    loop {
        let CollectionRead::Page { next_cursor, .. } =
            changed.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("page")
        };
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(changed.finish().await.unwrap_err().code, ErrorCode::Unsupported);
}

fn point_source(dataset: SnapshotDataset, reply: crate::read::ReadResult<Vec<Rec>>) -> StateSource<FakeRead> {
    let replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock(false, "2020-01-01T06:02:00Z")]),
        Ok(vec![pressure(false)]),
        reply,
        Ok(vec![clock(false, "2020-01-01T06:02:01Z")]),
        Ok(vec![identity()]),
    ];
    let mut request = request();
    request.selection = CollectionSelection::Snapshot { dataset };
    StateSource::with_io(FakeRead::new(replies), &request, "fixture.invalid".into(), 1433).unwrap()
}

#[tokio::test]
async fn retiring_description_does_not_authorize_a_second_native_read_on_the_same_snapshot() {
    let mut source =
        StateSource::with_io(FakeRead::new(vec![]).pause_on_query(1), &request(), "fixture.invalid".into(), 1433)
            .unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), source.describe()).await.is_err());
    assert_eq!(source.query_count(), 1);
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.closed, 1);
}

#[tokio::test]
async fn buffered_snapshot_lifecycle_never_reopens_closed_or_failed_work_or_repeats_verification() {
    let mut source = point_source(SnapshotDataset::WaitCounters, Ok(Vec::new()));
    source.describe().await.unwrap();
    let calls = source.query_count();
    assert!(source.finish().await.is_err(), "drain the explicit empty page before verification");
    assert_eq!(source.query_count(), calls);
    source.read_page(TelemetryStream::WaitCounterSamples, None).await.unwrap();
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    let calls = source.query_count();
    assert!(source.finish().await.is_err());
    assert!(source.describe().await.is_err());
    assert!(source.read_page(TelemetryStream::WaitCounterSamples, None).await.is_err());
    assert_eq!(source.query_count(), calls);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), calls);
    let mut source = point_source(SnapshotDataset::WaitCounters, Ok(Vec::new()));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), 0);
}

#[tokio::test]
async fn wait_snapshots_export_native_types_and_counter_semantics_with_per_source_closure() {
    let row = |wait: &str| {
        Rec::of(&[
            ("wait_type", Val::Text(wait.into())),
            ("waiting_tasks_count", Val::Int(3)),
            ("wait_time_ms", Val::Int(9_007_199_254_740_993)),
            ("signal_wait_time_ms", Val::Int(1)),
            ("max_wait_time_ms", Val::Int(10)),
        ])
    };
    let mut source = point_source(SnapshotDataset::WaitCounters, Ok(vec![row("WRITELOG"), row("SLEEP_TASK")]));
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.dataset, SnapshotDataset::WaitCounters);
    assert_eq!(snapshot.sources[0].records, 2);
    assert!(snapshot.sources[0].complete_scope);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::WaitCounterSamples, None).await.unwrap()
    else {
        panic!("page")
    };
    assert!(next_cursor.is_none());
    assert_eq!(records.len(), 2);
    let capture = CaptureIdentity { observation_id: "wait-fixture".into(), revision: None };
    let mut ids = std::collections::BTreeSet::new();
    for record in records {
        let exported = ExportedRecord::new(&description, &capture, record).unwrap();
        assert!(ids.insert(exported.record_id.unwrap()));
        let TelemetryRecord::WaitCounter(counter) = exported.record else { panic!("native wait counter") };
        assert_eq!(counter.wait_time_ms, "9007199254740993");
        assert_eq!(counter.reset, culprit_core::ResetMarker::Unknown);
    }
    assert_eq!(TelemetryStream::WaitCounterSamples.semantics(), TelemetrySemantics::CumulativeCounter);
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert_eq!(source.query_count(), 7);
}

#[tokio::test]
async fn failed_wait_counter_reads_preserve_unknown_coverage_without_claiming_a_clean_database() {
    let mut source = point_source(SnapshotDataset::WaitCounters, Err(ReadError::Stopped(CollectionStop::RowLimit)));
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 0);
    assert_eq!(snapshot.sources[0].state, SourceState::Unavailable);
    assert_eq!(snapshot.sources[0].stopped, Some(CollectionStop::RowLimit));
    assert!(!snapshot.sources[0].complete_scope);
    assert!(
        matches!(source.read_page(TelemetryStream::WaitCounterSamples, None).await.unwrap(), CollectionRead::Page { records, next_cursor: None } if records.is_empty())
    );
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
}

#[tokio::test]
async fn replication_snapshots_export_native_links_with_dataset_and_source_ownership() {
    let mut source = point_source(SnapshotDataset::Replication, Ok(vec![crate::replication::tests::geo()]));
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!((snapshot.dataset, snapshot.records), (SnapshotDataset::Replication, 1));
    assert_eq!(snapshot.sources[0].source, culprit_core::source_id::REPLICATION);
    assert!(snapshot.sources[0].complete_scope);
    assert_eq!(TelemetryStream::ReplicationSamples.semantics(), TelemetrySemantics::Sample);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ReplicationSamples, None).await.unwrap()
    else {
        panic!("replication page")
    };
    assert!(next_cursor.is_none());
    assert_eq!(records.len(), 1);
    let capture = CaptureIdentity { observation_id: "replication-fixture".into(), revision: None };
    let exported = ExportedRecord::new(&description, &capture, records[0].clone()).unwrap();
    assert!(exported.record_id.is_some());
    let mut wrong_dataset = description.clone();
    let SourceObservation::Snapshot(snapshot) = &mut wrong_dataset.observation else { panic!("snapshot") };
    snapshot.dataset = SnapshotDataset::WaitCounters;
    assert!(ExportedRecord::new(&wrong_dataset, &capture, records[0].clone()).is_err());
    let mut wrong_source = records[0].clone();
    let TelemetryRecord::Replication(row) = &mut wrong_source else { panic!("replication record") };
    row.source = "another-source".into();
    assert!(ExportedRecord::new(&description, &capture, wrong_source).is_err());
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn empty_replication_is_established_only_by_a_successful_native_read() {
    for failed in [false, true] {
        let reply = if failed {
            Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture denied")))
        } else {
            Ok(vec![])
        };
        let mut source = point_source(SnapshotDataset::Replication, reply);
        let description = source.describe().await.unwrap();
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.records, 0);
        assert_eq!(snapshot.sources[0].complete_scope, !failed);
        assert_eq!(snapshot.sources[0].state, if failed { SourceState::Unavailable } else { SourceState::Empty });
        assert_eq!(snapshot.sources[0].error_code, if failed { Some(ErrorCode::PermissionDenied) } else { None });
        assert!(matches!(source.read_page(TelemetryStream::ReplicationSamples, None).await.unwrap(),
            CollectionRead::Page {records, next_cursor: None} if records.is_empty()));
        assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    }
}

#[tokio::test]
async fn unassigned_queue_scope_preserves_raw_work_without_publishing_complete_database_absence() {
    let queued = replace(&crate::queues::tests::row(), &[("unattributed_count", Val::Int(1))]);
    let mut source = StateSource::with_io(
        FakeRead::new(vec![Ok(vec![pressure(false)]), Ok(vec![queued])]),
        &request(),
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    source.identity = Some(database_context());
    let coverage = source.read_families(&[point::Family::Queue(QueueKind::WorkerTasks)], true).await.unwrap();
    let coverage = &coverage[0];
    assert_eq!(coverage.state, SourceState::Partial);
    assert!(!coverage.complete_scope);
    assert_eq!(coverage.records, 1);
    let TelemetryRecord::Operational(row) = &source.rows[0] else { panic!("operational") };
    let OperationalFact::Queue(sample) = &row.fact else { panic!("queue") };
    assert_eq!(sample.count, "0");
    assert_eq!(sample.unattributed_count, "1");
}

#[tokio::test]
async fn settings_use_one_point_profile_with_source_coverage_and_stable_capture_ids() {
    use crate::settings::{
        Kind,
        tests::{database, options, scoped},
    };
    let mut replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
    ];
    for row in [database(), scoped(), options()] {
        replies.extend([Ok(vec![pressure(false)]), Ok(vec![row])]);
    }
    replies.extend([Ok(vec![clock(true, "2020-01-01T06:02:01Z")]), Ok(vec![identity()])]);
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget { page_rows: 10, ..Default::default() },
    };
    let mut source = StateSource::with_io(FakeRead::new(replies), &request, "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 8);
    assert_eq!(snapshot.sources.len(), 4);
    assert_eq!(
        snapshot.sources.iter().find(|row| row.source == Kind::Server.source().name).unwrap().state,
        SourceState::NotApplicable
    );
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::SettingObservations, None).await.unwrap()
    else {
        panic!("page")
    };
    assert!(next_cursor.is_none());
    let capture = CaptureIdentity { observation_id: "settings-fixture".into(), revision: None };
    let ids = records
        .iter()
        .map(|record| ExportedRecord::new(&description, &capture, record.clone()).unwrap().record_id.unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 8);
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn settings_flattened_from_one_native_row_cannot_overrun_the_total_snapshot_budget() {
    let request = CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Settings },
        budget: CollectionBudget { max_snapshot_rows: 1, ..Default::default() },
    };
    let mut source = StateSource::with_io(
        FakeRead::new(vec![Ok(vec![pressure(false)]), Ok(vec![crate::settings::tests::database()])]),
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    source.identity = Some(database_context());
    let coverage =
        source.read_families(&[point::Family::Settings(crate::settings::Kind::Database)], true).await.unwrap();
    assert!(source.rows.is_empty());
    assert_eq!(coverage[0].state, SourceState::Unavailable);
    assert_eq!(coverage[0].stopped, Some(CollectionStop::RowLimit));
    assert!(!coverage[0].complete_scope);
    assert_eq!(source.query_count(), 2);
}

#[tokio::test]
async fn slow_cache_profile_owns_its_counts_and_bytes_without_joining_fast_operational_reads() {
    let mut source = point_source(SnapshotDataset::PlanCacheSummary, Ok(vec![crate::cache_summary::tests::row()]));
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources.len(), 1);
    assert_eq!(snapshot.records, 6);
    assert_eq!(snapshot.sources[0].source, crate::sources::PLAN_CACHE_USE.name);
    assert!(snapshot.sources[0].complete_scope);
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let CollectionRead::Page { records: page, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!("page")
        };
        assert!(page.len() <= 2);
        records.extend(page);
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(records.len(), 6);
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert!(records.into_iter().all(|record| {
        ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "cache-fixture".into(), revision: None },
            record,
        )
        .is_ok()
    }));
    assert!(
        !point::families(SnapshotDataset::Operational)
            .unwrap()
            .iter()
            .any(|family| matches!(family, point::Family::PlanCacheSummary))
    );
}

#[tokio::test]
async fn a_failed_tuning_source_preserves_successful_siblings_and_their_individual_coverage() {
    use crate::query_store::tuning::native::tests::{forced_row, layout_row, option_row};
    let mut request = request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning };
    let replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
        Ok(vec![pressure(false)]),
        Ok(vec![layout_row()]),
        Ok(vec![pressure(false)]),
        Ok(vec![option_row()]),
        Ok(vec![pressure(false)]),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture denied"))),
        Ok(vec![pressure(false)]),
        Ok(vec![forced_row()]),
        Ok(vec![pressure(false)]),
        Ok(vec![]),
        Ok(vec![clock(true, "2020-01-01T06:02:01Z")]),
        Ok(vec![identity()]),
    ];
    let mut source = StateSource::with_io(FakeRead::new(replies), &request, "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources.len(), 4);
    assert_eq!(snapshot.sources[1].state, SourceState::Unavailable);
    assert!(!snapshot.sources[1].complete_scope);
    assert_eq!(snapshot.sources[3].state, SourceState::Empty);
    assert!(snapshot.sources[3].complete_scope);
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::TuningObservations, None).await.unwrap()
    else {
        panic!("two records")
    };
    assert_eq!(records.len(), 2);
    assert!(records.into_iter().all(|record| {
        ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "tuning-fixture".into(), revision: None },
            record,
        )
        .is_ok()
    }));
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
}

#[tokio::test]
async fn failed_tuning_layout_never_retries_metadata_or_runs_unusable_source_or_pressure_queries() {
    let mut request = request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning };
    let replies = vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
        Ok(vec![pressure(false)]),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "fixture metadata denied"))),
        Ok(vec![clock(true, "2020-01-01T06:02:01Z")]),
        Ok(vec![identity()]),
    ];
    let mut source = StateSource::with_io(FakeRead::new(replies), &request, "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.records, 0);
    assert_eq!(snapshot.sources.len(), 4);
    assert!(snapshot.sources.iter().all(|source| source.state == SourceState::Unavailable && !source.complete_scope));
    assert_eq!(source.io.calls.len(), 6);
    assert!(
        matches!(source.read_page(TelemetryStream::TuningObservations,None).await.unwrap(),CollectionRead::Page{records,next_cursor:None} if records.is_empty())
    );
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert_eq!(source.io.calls.len(), 7);
}

fn terminal_failures() -> Vec<culprit_core::Error> {
    [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::QueryLimit,
        CollectionStop::ByteLimit,
        CollectionStop::PageLimit,
        CollectionStop::Pressure,
        CollectionStop::PressureUnavailable,
        CollectionStop::SourceChanged,
    ]
    .into_iter()
    .map(CollectionStop::error)
    .chain(
        [ErrorCode::Timeout, ErrorCode::AuthFailed, ErrorCode::ConnectionFailed]
            .into_iter()
            .map(|code| culprit_core::Error::new(code, "fixture native read failed")),
    )
    .collect()
}

#[tokio::test]
async fn terminal_instance_reads_stop_before_sibling_sources_and_cannot_publish_staged_records() {
    use crate::instance::test_support::{epoch, metadata};
    let request = CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("fixture-machine").qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new("fixture-machine\\NATIVE").qualified("kind", "sql-server-instance"),
            },
        },
        ..request()
    };
    for error in terminal_failures() {
        let mut source = StateSource::with_io(
            FakeRead::new(vec![
                Ok(vec![metadata()]),
                Ok(vec![epoch(100)]),
                Ok(vec![clock(false, "2020-01-01T06:02:00Z")]),
                Ok(vec![space_row()]),
                Err(ReadError::Source(error.clone())),
            ]),
            &request,
            "unused-member-route.invalid".into(),
            1433,
        )
        .unwrap();
        let actual = source.describe().await.unwrap_err();
        assert_eq!(actual.code, error.code);
        assert_eq!(actual.detail, error.detail);
        assert_eq!(source.rows.len(), 1, "the earlier native fact remains provisional");
        assert!(source.description.is_none());
        assert!(source.describe().await.is_err());
        assert!(source.finish().await.is_err());
        assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
        assert_eq!(source.query_count(), 5, "no sibling, clock, pressure or identity query follows the stop");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.io.closed, 1);
    }
}

#[tokio::test]
async fn terminal_tuning_layout_reads_stop_before_follow_up_sources_or_verification_queries() {
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Tuning },
        ..request()
    };
    for error in terminal_failures() {
        let mut source = StateSource::with_io(
            FakeRead::new(vec![
                Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
                Ok(vec![identity()]),
                Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
                Ok(vec![pressure(false)]),
                Err(ReadError::Source(error.clone())),
            ]),
            &request,
            "fixture.invalid".into(),
            1433,
        )
        .unwrap();
        let actual = source.describe().await.unwrap_err();
        assert_eq!(actual.code, error.code);
        assert_eq!(actual.detail, error.detail);
        assert!(source.description.is_none());
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count(), 5);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn terminal_optional_index_associations_stop_before_the_final_clock_and_identity_queries() {
    let request = CollectionRequest {
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::MissingIndexes },
        ..request()
    };
    for error in terminal_failures() {
        let mut source = StateSource::with_io(
            FakeRead::new(vec![
                Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
                Ok(vec![identity()]),
                Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
                Ok(vec![pressure(false)]),
                Ok(vec![crate::missing_indexes::tests::row()]),
                Err(ReadError::Source(error.clone())),
            ]),
            &request,
            "fixture.invalid".into(),
            1433,
        )
        .unwrap();
        let actual = source.describe().await.unwrap_err();
        assert_eq!(actual.code, error.code);
        assert_eq!(actual.detail, error.detail);
        assert!(source.description.is_none());
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count(), 6);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn total_snapshot_row_exhaustion_stops_without_another_native_probe_or_final_clock() {
    let request =
        CollectionRequest { budget: CollectionBudget { max_snapshot_rows: 1, ..Default::default() }, ..request() };
    let mut source = StateSource::with_io(
        FakeRead::new(vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![identity()]),
            Ok(vec![clock(true, "2020-01-01T06:02:00Z")]),
            Ok(vec![pressure(false)]),
            Ok(vec![space_row()]),
        ]),
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::RowLimit);
    assert_eq!(source.rows.len(), 1);
    assert!(source.description.is_none());
    assert!(source.finish().await.is_err());
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
    assert_eq!(source.query_count(), 5);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
