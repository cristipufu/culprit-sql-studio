//! Lifecycle failures are checked at the adapter boundary, including retired native futures.
use super::*;
use crate::{
    resources::test_support::{FakeRead, latest_row, replace},
    value::Val,
};
use culprit_core::telemetry::*;

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: culprit_core::telemetry::CollectionSubject::Database,
        selection: CollectionSelection::QueryHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T07:00:00Z".into(),
        },
        budget: Default::default(),
    }
}
fn pressure() -> Rec {
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
fn description_rows() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![pressure()]),
        Ok(vec![Rec::of(&[
            ("actual_state_desc", Val::Text("READ_WRITE".into())),
            ("query_capture_mode_desc", Val::Text("AUTO".into())),
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
