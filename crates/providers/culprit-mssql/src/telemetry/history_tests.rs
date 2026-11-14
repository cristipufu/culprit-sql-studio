//! Lifecycle failures are checked at the adapter boundary, including retired native futures.
use super::*;
use crate::{
    resources::test_support::{FakeRead, latest_row, replace},
    value::Val,
};
use culprit_core::telemetry::*;

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
pub(super) fn description_rows() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![pressure()]),
        Ok(vec![Rec::of(&[
            ("actual_state_desc", Val::Text("READ_WRITE".into())),
            ("query_capture_mode_desc", Val::Text("AUTO".into())),
            ("variant_families", Val::Int(1)),
            ("wait_stats_capture_mode_desc", Val::Text("ON".into())),
        ])]),
        Ok(vec![Rec::of(&[
            ("database_name", Val::Text("fixture".into())),
            ("incarnation", Val::Text("logical".into())),
            ("physical_incarnation", Val::Text("physical".into())),
            ("updateability", Val::Text("READ_WRITE".into())),
        ])]),
        Ok(vec![Rec::of(&[
            ("id", Val::Int(1)),
            ("starts", Val::Text("2020-01-01T06:00:00Z".into())),
            ("ends", Val::Text("2020-01-01T07:00:00Z".into())),
        ])]),
        Ok(vec![Rec::of(&[("replica_group_id", Val::Int(1))])]),
        Ok(vec![Rec::of(&[("plan_id", Val::Int(0))])]),
    ]
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
        let Ok(rows) = &mut replies[2] else { panic!("native settings") };
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
            replies[2] = Err(crate::read::ReadError::Stopped(CollectionStop::TimedOut));
        } else {
            let Ok(rows) = &mut replies[2] else { panic!("native settings") };
            rows[0] = replace(&rows[0], &[("wait_stats_capture_mode_desc", Val::Text("UNKNOWN".into()))]);
        }
        let mut source = Source::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap();
        let error = source.describe().await.unwrap_err();
        if timeout {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
        }
        assert_eq!(source.query_count(), 3, "no identity/page query after failed settings");
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), 3);
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
    let mut closing = description_rows().into_iter().skip(2).collect::<Vec<_>>();
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
