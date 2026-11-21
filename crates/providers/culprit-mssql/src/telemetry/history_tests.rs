//! Lifecycle failures are checked at the adapter boundary, including retired native futures.
use super::*;
use crate::{
    resources::test_support::{FakeRead, latest_row, replace},
    value::Val,
};
use culprit_core::telemetry::*;
mod boxed;
mod page_server;

#[test]
fn runtime_distributions_are_unsupported_at_the_provider_boundary_without_native_work() {
    use culprit_core::Provider;
    let provider = crate::MssqlProvider::new(&culprit_core::ConnectionSpec {
        engine: Some(culprit_core::Engine::Mssql),
        host: "127.0.0.1".into(),
        port: Some(1),
        database: Some("fixture".into()),
        user: Some("unused".into()),
        ..Default::default()
    })
    .unwrap();
    for subject in [
        CollectionSubject::Database,
        CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("native-namespace"),
                native: NativeIdentity::new("native-instance"),
            },
        },
    ] {
        let request = CollectionRequest {
            subject,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::RuntimeDistributions },
            budget: CollectionBudget::default(),
        };
        let error = provider.telemetry_source(&request).err().expect("no distribution source is implemented");
        assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
        assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
    }
}

pub(super) fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::QueryHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        budget: Default::default(),
    }
}
pub(super) fn pressure() -> Rec {
    replace(
        &latest_row("2020-01-01T06:01:59Z"),
        &[
            ("avg_cpu_percent", Val::Float(10.0)),
            ("avg_instance_cpu_percent", Val::Float(10.0)),
            ("max_worker_percent", Val::Float(10.0)),
            ("avg_data_io_percent", Val::Float(10.0)),
            ("avg_log_write_percent", Val::Float(10.0)),
        ],
    )
}
pub(super) fn native_platform(edition: i64) -> Rec {
    replace(&crate::instance::test_support::metadata(), &[("edition", Val::Int(edition))])
}
pub(super) fn authority(edition: i64) -> Rec {
    replace(
        &crate::activity::database_context::tests::row(edition),
        &[
            ("database_name", Val::Text("fixture".into())),
            ("catalog_database_name", Val::Text("fixture".into())),
            ("incarnation", if edition == 5 { Val::Text("logical".into()) } else { Val::Null }),
            ("physical_incarnation", if edition == 5 { Val::Text("physical".into()) } else { Val::Null }),
        ],
    )
}
pub(super) fn options(edition: i64, partitioned: bool) -> Rec {
    Rec::of(&[
        ("engine_edition", Val::Int(edition)),
        ("tempdb", Val::Int(1)),
        ("log_bytes", Val::Int(1)),
        ("replicas", Val::Int(i64::from(partitioned))),
        ("waits", Val::Int(1)),
        ("metadata_visibility", Val::Int(1)),
        ("actual_state_desc", Val::Text("READ_WRITE".into())),
        ("query_capture_mode_desc", Val::Text("AUTO".into())),
        ("variants", Val::Int(1)),
        ("page_server_read_availability", Val::Int(0)),
        ("wait_stats_capture_mode_desc", Val::Text("ON".into())),
    ])
}
pub(super) fn description_rows() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![native_platform(5)]),
        Ok(vec![authority(5)]),
        Ok(vec![pressure()]),
        Ok(vec![options(5, true)]),
        Ok(vec![Rec::of(&[
            ("id", Val::Int(1)),
            ("starts", Val::Text("2020-01-01T06:00:00Z".into())),
            ("ends", Val::Text("2020-01-01T07:00:00Z".into())),
        ])]),
        Ok(vec![crate::query_store::replicas::tests::row(1, 1, None)]),
        Ok(vec![Rec::of(&[("plan_id", Val::Int(0))])]),
    ]
}
pub(super) fn closing_rows(availability: i64) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let mut rows = description_rows();
    let Ok(options) = &mut rows[3] else { panic!() };
    options[0] = replace(&options[0], &[("page_server_read_availability", Val::Int(availability))]);
    // Existing final pressure checkpoint is supplied by the caller; metadata precedes final held-owner recheck.
    let mut current = rows.drain(3..).collect::<Vec<_>>();
    current.push(Ok(vec![authority(5)]));
    current
}

#[tokio::test]
async fn readable_secondary_exports_exact_native_history_without_claiming_local_capture_when_read_only() {
    for (mode, permitted) in [("READ_CAPTURE_SECONDARY", "true"), ("READ_ONLY", "false")] {
        let mut replies = description_rows();
        let Ok(options) = &mut replies[3] else { unreachable!() };
        options[0] = replace(&options[0], &[("actual_state_desc", Val::Text(mode.into()))]);
        let Ok(identity) = &mut replies[1] else { unreachable!() };
        identity[0] = replace(&identity[0], &[("updateability", Val::Text("READ_ONLY".into()))]);
        replies[5] = Ok(vec![crate::query_store::replicas::tests::row(7, 2, None)]);
        replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
        replies.extend([Ok(vec![pressure()]), Ok(vec![crate::query_store::tests::runtime_row(3000)])]);
        let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        assert_eq!(source.query_count(), 0);
        let description = source.describe().await.unwrap();
        assert_eq!(description.partition, SourcePartition::Target);
        let history = description.query_history().unwrap();
        assert_eq!(history.native_properties["actual_state"], mode);
        assert_eq!(history.native_properties["local_capture_permitted"], permitted);
        assert_eq!(history.dimensions["replica_group_id"], ["7"]);
        assert_eq!(
            history.population_binding,
            QueryHistoryPopulationBinding::Dimensions {
                bindings: vec![QueryHistoryPopulationBindingEntry {
                    population: NativeIdentity::new("7"),
                    qualifiers: [("replica_group_id".into(), "7".into())].into(),
                }],
            }
        );
        let catalog = history.population_catalog.as_ref().unwrap();
        let groups: serde_json::Value =
            serde_json::from_str(&catalog.populations[0].native_properties["role_history"]).unwrap();
        assert_eq!(groups[0]["role_type"], 2);
        assert!(groups[0]["replica_name"].is_null());
        let CollectionRead::Page { records, next_cursor: None } =
            source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap()
        else {
            panic!("retained native history")
        };
        let TelemetryRecord::QueryRuntime(runtime) = &records[0] else { panic!("native runtime") };
        assert_eq!(runtime.key.native.qualifiers["replica_group_id"], "7");
        assert_eq!(runtime.key.native.qualifiers["execution_type"], "3");
        assert_eq!(runtime.executions, "1");
        assert_eq!(runtime.metric(&RuntimeMetricKey::Duration).unwrap().average, Some(3000.0));
        assert_eq!(source.query_count(), 9, "same bounded description plus one pressure/page read");
        assert!(source.finish().await.is_err(), "unread waits/definitions cannot produce complete history");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

fn identified_runtime(group: i64, duration: i64) -> Rec {
    let runtime = crate::query_store::tests::runtime_row(duration);
    let metadata = crate::query_store::tests::summary_row();
    let mut fields = runtime.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    for (name, value) in metadata.columns() {
        if !fields.iter().any(|(key, _)| *key == name) {
            fields.push((name, value.clone()));
        }
    }
    replace(&Rec::of(&fields), &[("replica_group_id", Val::Int(group))])
}

fn identified_wait(group: i64, total: i64) -> Rec {
    let runtime = identified_runtime(group, 1_000);
    let mut fields = runtime.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("wait_category", Val::Int(3)),
        ("total_query_wait_time_ms", Val::Int(total)),
        ("avg_query_wait_time_ms", Val::Float(total as f64)),
        ("min_query_wait_time_ms", Val::Int(total)),
        ("max_query_wait_time_ms", Val::Int(total)),
        ("last_query_wait_time_ms", Val::Null),
        ("stdev_query_wait_time_ms", Val::Null),
        ("wait_category_desc", Val::Null),
        ("execution_type_desc", Val::Null),
    ]);
    replace(&Rec::of(&fields), &[("row_id", Val::Int(100 + group))])
}

#[tokio::test]
async fn native_group_binding_reuses_the_catalog_read_and_keeps_query_runtime_and_wait_populations_exact() {
    let mut replies = description_rows();
    replies[5] = Ok(vec![
        crate::query_store::replicas::tests::row(7, 1, Some("same-role-member")),
        crate::query_store::replicas::tests::row(1, 1, Some("same-role-member")),
    ]);
    replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
    replies.extend([
        Ok(vec![pressure()]),
        Ok(vec![identified_runtime(1, 1_000), identified_runtime(7, 3_000)]),
        Ok(vec![pressure()]),
        Ok(vec![identified_wait(1, 7), identified_wait(7, 90)]),
    ]);
    let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(source.query_count(), 7, "binding is computed from the existing native catalog observation");
    let history = description.query_history().unwrap();
    let QueryHistoryPopulationBinding::Dimensions { bindings } = &history.population_binding else {
        panic!("source-owned native bindings")
    };
    assert_eq!(bindings.len(), 2, "equal role/name metadata cannot merge native execution groups");
    assert_eq!(bindings[0].population, NativeIdentity::new("1"));
    assert_eq!(bindings[1].population, NativeIdentity::new("7"));
    let CollectionRead::Page { records: runtime, next_cursor: None } =
        source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap()
    else {
        panic!("native runtime page")
    };
    let CollectionRead::Page { records: waits, next_cursor: None } =
        source.read_page(TelemetryStream::QueryWaits, None).await.unwrap()
    else {
        panic!("native wait page")
    };
    for (index, (runtime, wait)) in runtime.iter().zip(&waits).enumerate() {
        let TelemetryRecord::QueryRuntime(runtime) = runtime else { panic!("runtime") };
        let TelemetryRecord::QueryWaits(wait) = wait else { panic!("wait") };
        assert_eq!(runtime.key.references, wait.key.references);
        assert!(runtime.key.references.is_resolved());
        assert_eq!(runtime.key.cohort, wait.key.cohort);
        assert_eq!(
            history.execution_population(&runtime.key.native.qualifiers).unwrap(),
            QueryExecutionPopulation::Population(&bindings[index].population)
        );
        assert_eq!(
            history.execution_population(&wait.key.native.qualifiers).unwrap(),
            QueryExecutionPopulation::Population(&bindings[index].population)
        );
        assert_eq!(runtime.metric(&RuntimeMetricKey::Duration).unwrap().average, Some([1_000.0, 3_000.0][index]));
        assert_eq!(wait.total.integer(), Some([7, 90][index]));
    }
    assert_eq!(runtime[0].measurement_key().unwrap().references, runtime[1].measurement_key().unwrap().references);
    assert_eq!(source.query_count(), 11, "description plus the existing two bounded pressure/page pairs");
    assert_eq!(
        source.session.calls.iter().filter(|call| call.sql == crate::query_store::catalog_sql::REPLICAS).count(),
        1
    );
    assert_eq!(
        source.session.calls[8].sql,
        crate::query_store::catalog_sql::runtime(None, &source.description.as_ref().unwrap().layout)
    );
    assert_eq!(
        source.session.calls[10].sql,
        crate::query_store::catalog_sql::waits(None, &source.description.as_ref().unwrap().layout)
    );
    assert!(source.finish().await.is_err(), "unread definitions still cannot establish a complete capture");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_undeclared_native_group_stops_before_other_pages_instead_of_becoming_unknown_or_unpartitioned() {
    let mut replies = description_rows();
    replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
    replies.extend([Ok(vec![pressure()]), Ok(vec![identified_runtime(8, 1_000)])]);
    let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.query_history().unwrap().is_valid());
    assert!(matches!(
        source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap(),
        CollectionRead::Stopped(CollectionStop::SourceChanged)
    ));
    assert!(source.read_page(TelemetryStream::QueryWaits, None).await.is_err());
    assert_eq!(source.query_count(), 9);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn query_history_never_verifies_unread_streams_and_closed_sources_cannot_reopen() {
    let mut source =
        Source::with_io(FakeRead::new(description_rows()), &request(), "fixture.invalid".into(), 1433).unwrap();
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 0);
    source.describe().await.unwrap();
    let reads = source.query_count();
    assert!(source.finish().await.is_err());
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), reads);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert!(source.describe().await.is_err());
    assert!(source.read_page(TelemetryStream::QueryRuntime, None).await.is_err());
    assert_eq!(source.query_count(), reads);
}
#[tokio::test]
async fn retiring_query_description_or_native_page_cannot_authorize_another_read() {
    for pause_at in [1, 9] {
        let mut replies = description_rows();
        replies.push(Ok(vec![pressure()]));
        let mut io = FakeRead::new(replies).pause_on_query(pause_at);
        io.shutdown = SourceShutdown::Unconfirmed;
        let mut source = Source::with_io(io, &request(), "fixture.invalid".into(), 1433).unwrap();
        let timeout = std::time::Duration::from_millis(1);
        if pause_at == 1 {
            assert!(tokio::time::timeout(timeout, source.describe()).await.is_err());
        } else {
            source.describe().await.unwrap();
            assert!(
                tokio::time::timeout(timeout, source.read_page(TelemetryStream::QueryRuntime, None)).await.is_err()
            );
        }
        assert!(source.describe().await.is_err());
        assert!(source.read_page(TelemetryStream::QueryRuntime, None).await.is_err());
        assert!(source.finish().await.is_err());
        assert_eq!(source.query_count() as usize, pause_at);
        assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    }
}

#[tokio::test]
async fn query_history_freezes_native_wait_semantics_without_an_additional_read() {
    for (native, capture) in [("ON", WaitCaptureState::Enabled), ("OFF", WaitCaptureState::Disabled)] {
        let mut replies = description_rows();
        let Ok(rows) = &mut replies[3] else { panic!("native settings") };
        rows[0] = replace(&rows[0], &[("wait_stats_capture_mode_desc", Val::Text(native.into()))]);
        if capture == WaitCaptureState::Disabled {
            replies.extend([
                Ok(vec![pressure()]),
                Ok(vec![Rec::of(&[
                    ("row_id", Val::Int(1)),
                    ("plan_id", Val::Int(0)),
                    ("replica_group_id", Val::Int(1)),
                    ("execution_type", Val::Int(0)),
                    ("wait_category", Val::Int(3)),
                    ("total_query_wait_time_ms", Val::Int(20)),
                    ("avg_query_wait_time_ms", Val::Float(20.0)),
                    ("min_query_wait_time_ms", Val::Int(20)),
                    ("max_query_wait_time_ms", Val::Int(20)),
                    ("last_query_wait_time_ms", Val::Null),
                    ("stdev_query_wait_time_ms", Val::Null),
                    ("wait_category_desc", Val::Null),
                    ("execution_type_desc", Val::Null),
                ])]),
            ]);
        }
        let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        let description = source.describe().await.unwrap();
        let semantics = description.query_history().unwrap().waits.as_ref().unwrap();
        assert_eq!(semantics, &crate::query_store::waits::semantics(capture));
        assert_eq!(source.query_count(), 7, "metadata must reuse the existing settings read");
        if capture == WaitCaptureState::Disabled {
            let CollectionRead::Page { records, next_cursor: None } =
                source.read_page(TelemetryStream::QueryWaits, None).await.unwrap()
            else {
                panic!("bounded retained native waits")
            };
            assert!(
                matches!(&records[0],TelemetryRecord::QueryWaits(wait) if wait.category=="3" && wait.total.integer()==Some(20))
            );
            assert_eq!(source.query_count(), 9, "one pressure read and one bounded retained-history page");
            assert!(source.finish().await.is_err());
        }
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn unrecognized_or_timed_out_wait_settings_cannot_become_disabled_or_read_further() {
    for timeout in [false, true] {
        let mut replies = description_rows();
        if timeout {
            replies[3] = Err(crate::read::ReadError::Stopped(CollectionStop::TimedOut));
        } else {
            let Ok(rows) = &mut replies[3] else { panic!("native settings") };
            rows[0] = replace(&rows[0], &[("wait_stats_capture_mode_desc", Val::Text("UNKNOWN".into()))]);
        }
        let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        let error = source.describe().await.unwrap_err();
        if timeout {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
        }
        assert_eq!(source.query_count(), 4, "no interval/page query after failed settings");
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 4);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_wait_capture_change_prevents_empty_native_streams_from_verifying_complete() {
    let mut replies = description_rows();
    replies.extend([
        Ok(vec![pressure()]),
        Ok(vec![]),
        Ok(vec![pressure()]),
        Ok(vec![]),
        Ok(vec![pressure()]),
        Ok(vec![pressure()]),
        Ok(vec![pressure()]),
    ]);
    let mut closing = closing_rows(0);
    let Ok(settings) = &mut closing[0] else { panic!("native settings") };
    settings[0] = replace(&settings[0], &[("wait_stats_capture_mode_desc", Val::Text("OFF".into()))]);
    replies.extend(closing);
    let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
    source.describe().await.unwrap();
    for stream in [
        TelemetryStream::QueryRuntime,
        TelemetryStream::QueryWaits,
        TelemetryStream::QueryDefinitions,
        TelemetryStream::PlanDefinitions,
    ] {
        let page = source.read_page(stream, None).await.unwrap();
        assert!(matches!(page, CollectionRead::Page { records, next_cursor: None } if records.is_empty()));
    }
    assert!(!source.finish().await.unwrap().is_verified());
    assert_eq!(source.query_count(), 19);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
