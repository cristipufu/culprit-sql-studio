use super::*;
use crate::{
    resources::test_support::*,
    value::{Rec, Val},
};
fn request() -> CollectionRequest {
    CollectionRequest {
        selection: CollectionSelection::ResourceHistory {
            from: "2020-01-01T06:00:00Z".into(),
            until: "2020-01-01T06:01:00Z".into(),
        },
        budget: CollectionBudget::default(),
    }
}
fn identity() -> Rec {
    Rec::of(&[
        ("database_name", Val::Text("fixture".into())),
        ("incarnation", Val::Text("logical-guid".into())),
        ("physical_incarnation", Val::Text("physical-guid".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
    ])
}
fn source(replies: Vec<crate::read::ReadResult<Vec<Rec>>>) -> ResourceSource<FakeRead> {
    ResourceSource::with_io(FakeRead::new(replies), &request(), "fixture.invalid".into(), 1433).unwrap()
}
#[tokio::test]
async fn paged_exports_validate_native_lookahead_and_verify_the_same_selection() {
    let mut source = source(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
        Ok(vec![row("2020-01-01T06:00:30Z")]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
    ]);
    source.request.budget.page_rows = 1;
    let description = source.describe().await.unwrap();
    assert_eq!(description.source, culprit_core::source_id::RESOURCES);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap()
    else {
        panic!("expected sample page")
    };
    assert_eq!(records.len(), 1);
    let cursor = next_cursor.unwrap();
    assert_eq!(cursor, "2020-01-01T06:00:15.000000000Z");
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::ResourceSamples, Some(&cursor)).await.unwrap()
    else {
        panic!("expected final page")
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    assert_eq!(source.session.closed, 1);
    assert_eq!(source.query_count(), 7);
}
#[tokio::test]
async fn invalid_native_lookahead_retires_export_without_authorizing_a_reread() {
    let mut source = source(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:15Z")]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
    ]);
    source.request.budget.page_rows = 1;
    source.describe().await.unwrap();
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    let calls = source.query_count();
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), calls);
}
#[tokio::test]
async fn changing_source_identity_or_inventory_cannot_verify_a_capture() {
    for (identity, inventory) in [
        (
            replace(&identity(), &[("physical_incarnation", Val::Text("different-physical-guid".into()))]),
            inventory_row(),
        ),
        (identity(), replace(&inventory_row(), &[("selected_last", Val::Text("2020-01-01T06:00:45Z".into()))])),
    ] {
        let mut source = source(vec![
            Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
            Ok(vec![self::identity()]),
            Ok(vec![inventory_row()]),
            Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
            Ok(vec![identity]),
            Ok(vec![inventory]),
        ]);
        source.describe().await.unwrap();
        source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap();
        assert_eq!(source.finish().await.unwrap(), culprit_core::telemetry::SourceCompletion::Changed);
    }
}
#[tokio::test]
async fn export_primary_policy_is_explicit_and_does_not_relabel_secondary_samples() {
    let mut source = source(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![replace(&inventory_row(), &[("replica_role", Val::Int(1))])]),
    ]);
    let error = source.describe().await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
}
#[tokio::test]
async fn empty_retention_does_not_claim_complete_coverage() {
    let empty = replace(
        &inventory_row(),
        &[
            ("available_from", Val::Null),
            ("available_until", Val::Null),
            ("selected_count", Val::Int(0)),
            ("retained_count", Val::Int(0)),
            ("selected_first", Val::Null),
            ("selected_last", Val::Null),
            ("replica_role", Val::Null),
        ],
    );
    let mut source =
        source(vec![Ok(vec![Rec::of(&[("edition", Val::Int(5))])]), Ok(vec![identity()]), Ok(vec![empty])]);
    let description = source.describe().await.unwrap();
    assert!(!description.observation.covers_request());
}

#[tokio::test]
async fn a_replica_change_in_the_native_page_cannot_publish_or_advance_an_export() {
    let mut source = source(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
        Ok(vec![replace(&row("2020-01-01T06:00:15Z"), &[("replica_role", Val::Int(1))])]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
    ]);
    source.describe().await.unwrap();
    let error = source.read_page(TelemetryStream::ResourceSamples, None).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    let calls = source.query_count();
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), calls);
}

#[tokio::test]
async fn premature_or_repeated_verification_and_closed_observations_never_send_sql() {
    let mut source = source(vec![
        Ok(vec![Rec::of(&[("edition", Val::Int(5))])]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
        Ok(vec![identity()]),
        Ok(vec![inventory_row()]),
    ]);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 0);
    source.describe().await.unwrap();
    let calls = source.query_count();
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), calls);
    source.read_page(TelemetryStream::ResourceSamples, None).await.unwrap();
    source.finish().await.unwrap();
    let calls = source.query_count();
    assert!(source.finish().await.is_err());
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    assert_eq!(source.query_count(), calls);
    source.close().await;
    assert!(source.describe().await.is_err());
    assert_eq!(source.query_count(), calls);
}

#[tokio::test]
async fn retiring_resource_description_requires_explicit_native_close() {
    let mut io = FakeRead::new(vec![]).pause_on_query(1);
    io.shutdown = SourceShutdown::Unconfirmed;
    let mut source = ResourceSource::with_io(io, &request(), "fixture.invalid".into(), 1433).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1), source.describe()).await.is_err());
    assert!(source.describe().await.is_err());
    assert!(source.read_page(TelemetryStream::ResourceSamples, None).await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 1);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
}
