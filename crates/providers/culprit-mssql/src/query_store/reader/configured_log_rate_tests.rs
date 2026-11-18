use super::*;
use crate::{
    query_store::window_sql::{
        WindowVerification,
        tests::{azure_row, layout_row},
    },
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn metadata() -> Rec {
    let original = azure_row();
    let mut fields = original.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("configured_log_database_id", Val::Int(7)),
        ("configured_log_group_id", Val::Null),
        ("configured_log_workload_group", Val::Int(9_007_199_254_740_993)),
        ("configured_log_resource_pool", Val::Int(0)),
        ("configured_log_instance", Val::Null),
    ]);
    Rec::of(&fields)
}

fn configured_layout(available: i64) -> Rec {
    let original = replace(&layout_row(), &[("engine_edition", Val::Int(5))]);
    let mut fields = original.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.push(("configured_log_rate_capability", Val::Int(available)));
    Rec::of(&fields)
}

#[tokio::test]
async fn selected_scan_configuration_reuses_native_metadata_and_is_retained_only_with_settled_owner() {
    for shutdown in [SourceShutdown::Quiescent, SourceShutdown::Unconfirmed] {
        let initial = metadata();
        let final_row = replace(
            &initial,
            &[
                ("window_clock", Val::Text("2020-01-01T07:00:01Z".into())),
                ("configured_log_workload_group", Val::Int(1)),
            ],
        );
        let layout = configured_layout(1);
        let mut io = FakeRead::new(vec![Ok(vec![layout]), Ok(vec![initial]), Ok(vec![final_row])]);
        io.shutdown = shutdown;
        let mut reader = Reader::new(io);
        reader.request_configured_log_rates().unwrap();
        reader.window(60).await.unwrap();
        assert!(
            reader.configured_log_rates(SourceShutdown::Quiescent, "native-route.invalid", 1433).unwrap().is_none(),
            "initial metadata alone cannot borrow the later scope proof"
        );
        assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::Unchanged);
        let closed = reader.close().await;
        let result = reader.configured_log_rates(closed, "native-route.invalid", 1433);
        if shutdown == SourceShutdown::Unconfirmed {
            assert_eq!(CollectionStop::from_error(&result.unwrap_err()), CollectionStop::CancellationUnconfirmed);
        } else {
            let read = result.unwrap().unwrap();
            assert!(read.is_valid_read(true));
            assert_eq!(read.observation.unwrap().workload_group_bytes_per_second.as_deref(), Some("9007199254740993"));
            assert!(!read.context.has_qualified_local_owner());
        }
        assert_eq!(reader.io.calls.len(), 3);
        assert!(reader.io.calls[1].sql.contains("primary_max_log_rate"));
        assert!(!reader.io.calls[2].sql.contains("primary_max_log_rate"));
        assert_eq!(reader.io.closed, 1);
    }
}

#[tokio::test]
async fn hidden_or_changed_optional_capability_keeps_original_history_without_a_cap_projection_or_retry() {
    for available in [0, 2] {
        let initial = azure_row();
        let final_row = replace(&initial, &[("window_clock", Val::Text("2020-01-01T07:00:01Z".into()))]);
        let mut reader = Reader::new(FakeRead::new(vec![
            Ok(vec![configured_layout(available)]),
            Ok(vec![initial]),
            Ok(vec![final_row]),
        ]));
        reader.request_configured_log_rates().unwrap();
        assert!(reader.window(60).await.unwrap().retained);
        assert_eq!(reader.verify_window().await.unwrap(), WindowVerification::Unchanged);
        let closed = reader.close().await;
        let result = reader.configured_log_rates(closed, "native-route.invalid", 1433);
        if available == 0 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err());
        }
        assert_eq!(reader.io.calls.len(), 3);
        assert!(!reader.io.calls[1].sql.contains("primary_max_log_rate"));
        assert!(!reader.io.calls[2].sql.contains("primary_max_log_rate"));
        assert_eq!(reader.io.closed, 1);
    }
}

#[tokio::test]
async fn terminal_capability_read_failure_stops_before_governance_or_runtime_body_queries() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        io.shutdown = if stop == CollectionStop::CancellationUnconfirmed {
            SourceShutdown::Unconfirmed
        } else {
            SourceShutdown::Quiescent
        };
        let mut reader = Reader::new(io);
        reader.request_configured_log_rates().unwrap();
        assert_eq!(CollectionStop::from_error(&reader.window(60).await.err().unwrap()), stop);
        assert_eq!(reader.io.calls.len(), 1);
        let shutdown = reader.close().await;
        let retained = reader.configured_log_rates(shutdown, "route.invalid", 1433);
        if shutdown == SourceShutdown::Quiescent {
            assert!(retained.unwrap().is_none());
        } else {
            assert_eq!(CollectionStop::from_error(&retained.unwrap_err()), CollectionStop::CancellationUnconfirmed);
        }
        assert_eq!(reader.io.calls.len(), 1);
    }
}
