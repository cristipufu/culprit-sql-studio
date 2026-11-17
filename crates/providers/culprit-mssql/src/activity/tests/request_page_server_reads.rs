use super::*;
use crate::activity::page_server_reads::{self, Availability};

fn page_fields(row: &Rec, capability: i64, value: Val) -> Rec {
    let mut fields = row
        .columns()
        .filter(|(name, _)| {
            !matches!(
                *name,
                "request_page_server_read_capability"
                    | "request_page_server_read_availability"
                    | "request_page_server_reads"
            )
        })
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    fields.extend([
        ("request_page_server_read_capability", Val::Int(capability)),
        ("request_page_server_read_availability", Val::Int(capability)),
        ("request_page_server_reads", value),
    ]);
    Rec::of(&fields)
}

#[test]
fn optional_native_layout_and_counters_keep_missing_null_zero_and_exact_bigint_distinct() {
    assert_eq!(page_server_reads::capability(&Rec::of(&[])).unwrap(), Availability::Unknown);
    assert_eq!(page_server_reads::count(&Rec::of(&[])).unwrap(), None);
    for (value, expected) in [
        (Val::Null, None),
        (Val::Int(0), Some("0")),
        (Val::Int(9007199254740993), Some("9007199254740993")),
        (Val::Int(i64::MAX), Some("9223372036854775807")),
    ] {
        assert_eq!(page_server_reads::count(&page_fields(&Rec::of(&[]), 1, value)).unwrap().as_deref(), expected);
    }
    for unavailable in [0, 2, 3] {
        assert_eq!(page_server_reads::count(&page_fields(&Rec::of(&[]), unavailable, Val::Null)).unwrap(), None);
        assert!(page_server_reads::count(&page_fields(&Rec::of(&[]), unavailable, Val::Int(0))).is_err());
    }
    for malformed in [Val::Int(-1), Val::Text("9223372036854775808".into()), Val::Float(1.0)] {
        assert!(page_server_reads::count(&page_fields(&Rec::of(&[]), 1, malformed)).is_err());
    }
    let bad = page_fields(&Rec::of(&[]), 4, Val::Null);
    assert!(page_server_reads::capability(&bad).is_err());
}

#[tokio::test]
async fn same_held_profile_exposes_request_count_once_without_borrowing_session_totals_or_extra_dispatch() {
    for pages in [0, 9007199254740993, i64::MAX] {
        let raw = page_fields(&metadata(42), 1, Val::Int(pages));
        let before = page_fields(&authority(&raw), 1, Val::Null);
        let mut source = reader(
            vec![Ok(vec![before.clone()]), Ok(vec![raw]), Ok(vec![before])],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let (_, records) = crate::telemetry::activity::capture(&mut source).await.unwrap();
        assert_eq!(records[0].request_counters.as_ref().unwrap().page_server_reads, Some(pages.to_string()));
        assert!(records[0].session_counters.page_server_reads.is_none());
        assert_eq!(source.query_count(), 3);
        assert!(source.io.calls[1].sql.contains("r.page_server_reads"));
        assert!(matches!(&source.io.calls[1].parameters[..], [Parameter::Integer(501), Parameter::Integer(1)]));
        assert!(source.io.calls.iter().all(|call| call.sql.contains("/* culprit */")));
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.io.closed, 1);
    }
}

#[tokio::test]
async fn unknown_unavailable_or_denied_metadata_never_promotes_a_later_capability_or_healthy_zero() {
    for capability in [0, 2, 3] {
        let raw = page_fields(&metadata(42), capability, Val::Null);
        let before = page_fields(&authority(&raw), capability, Val::Null);
        let mut source = reader(
            vec![Ok(vec![before.clone()]), Ok(vec![raw]), Ok(vec![before])],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let (_, records) = crate::telemetry::activity::capture(&mut source).await.unwrap();
        assert_eq!(records[0].request_counters.as_ref().unwrap().page_server_reads, None);
        assert_eq!(records[0].request_counters.as_ref().unwrap().cpu_ms.as_deref(), Some("10"));
        assert!(!source.io.calls[1].sql.contains("r.page_server_reads"));
        assert_eq!(source.query_count(), 3);
    }
    let denied = || Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "controlled metadata denial")));
    let mut source = reader(
        vec![denied(), Ok(vec![metadata(42)]), denied()],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (description, records) = crate::telemetry::activity::capture(&mut source).await.unwrap();
    assert!(!description.observation.activity().unwrap().coverage.complete_scope);
    assert!(records[0].request_counters.as_ref().unwrap().page_server_reads.is_none());
    assert_eq!(source.query_count(), 3);
}

#[tokio::test]
async fn a_changed_final_native_profile_refuses_the_original_positive_request_counter() {
    let raw = page_fields(&metadata(42), 1, Val::Int(9007199254740993));
    for final_capability in [0, 2, 3] {
        let initial = page_fields(&authority(&raw), 1, Val::Null);
        let final_row = page_fields(&authority(&raw), final_capability, Val::Null);
        let mut source = reader(
            vec![Ok(vec![initial]), Ok(vec![raw.clone()]), Ok(vec![final_row])],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let error = crate::telemetry::activity::capture(&mut source).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::SourceChanged)));
        assert_eq!(source.query_count(), 3);
    }
}

#[tokio::test]
async fn final_optional_authority_denial_withholds_only_the_unattested_optional_count_and_keeps_the_frame_gap() {
    let raw = page_fields(&metadata(42), 1, Val::Int(9007199254740993));
    let initial = page_fields(&authority(&raw), 1, Val::Null);
    let mut source = reader(
        vec![
            Ok(vec![initial]),
            Ok(vec![raw]),
            Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "controlled final metadata denial"))),
        ],
        ActivityScope::CurrentDatabase,
        IdentityDetail::AzureDatabase,
    );
    let (description, records) = crate::telemetry::activity::capture(&mut source).await.unwrap();
    assert_eq!(records.len(), 1);
    let counters = records[0].request_counters.as_ref().unwrap();
    assert!(counters.page_server_reads.is_none());
    assert_eq!(counters.cpu_ms.as_deref(), Some("10"));
    let coverage = &description.observation.activity().unwrap().coverage;
    assert!(!coverage.complete_scope);
    assert_eq!(coverage.error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(source.query_count(), 3);
}

#[tokio::test]
async fn context_free_metadata_can_observe_capability_without_rereading_or_inventing_a_counter() {
    let raw = page_fields(&metadata(42), 1, Val::Null);
    let mut source = reader(vec![Ok(vec![raw])], ActivityScope::Visible, IdentityDetail::Endpoint);
    let frame = source.observe().await.unwrap();
    assert_eq!(frame.context.request_page_server_read_capability, Availability::Available);
    assert_eq!(frame.records[0].facts.request_counters.as_ref().unwrap().page_server_reads, None);
    assert_eq!(source.query_count(), 1);
    assert!(!source.io.calls[0].sql.contains("r.page_server_reads"));
}

#[tokio::test]
async fn terminal_final_authority_stops_never_publish_a_first_point_count_or_retry() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let raw = page_fields(&metadata(42), 1, Val::Int(9007199254740993));
        let initial = page_fields(&authority(&raw), 1, Val::Null);
        let mut source = reader(
            vec![Ok(vec![initial]), Ok(vec![raw]), Err(ReadError::Stopped(stop))],
            ActivityScope::CurrentDatabase,
            IdentityDetail::AzureDatabase,
        );
        let error = crate::telemetry::activity::capture(&mut source).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(source.query_count(), 3);
        assert_eq!(source.io.closed, 0, "the capture caller retains the native shutdown owner");
        source.io.shutdown = SourceShutdown::Unconfirmed;
        assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
        assert_eq!(source.io.closed, 1);
    }
}

#[tokio::test]
async fn a_stricter_caller_query_budget_cannot_skip_the_final_profile_check_or_retry() {
    let raw = page_fields(&metadata(42), 1, Val::Int(9007199254740993));
    let initial = page_fields(&authority(&raw), 1, Val::Null);
    let mut source = ActivityReader::new(
        crate::resources::test_support::FakeRead::new(vec![Ok(vec![initial]), Ok(vec![raw])]).with_query_limit(2),
        ReadOptions {
            budget: CollectionBudget::default(),
            scope: ActivityScope::CurrentDatabase,
            identity: IdentityDetail::AzureDatabase,
        },
        "fixture.invalid".into(),
        1433,
    );
    let error = crate::telemetry::activity::capture(&mut source).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::QueryLimit);
    assert_eq!(source.query_count(), 2, "the rejected final check performs no native dispatch");
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.io.closed, 1);
}
