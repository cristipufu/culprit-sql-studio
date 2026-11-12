use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("created_without_offset", Val::Text("2026-11-10T07:00:00.123".into())),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("Child".into())),
        ("stats_id", Val::Int(3)),
        ("stats_name", Val::Text("owner_statistics".into())),
        ("auto_created", Val::Int(1)),
        ("user_created", Val::Int(0)),
        ("no_recompute", Val::Int(0)),
        ("is_memory_optimized", Val::Int(0)),
        ("properties_available", Val::Int(1)),
        ("last_updated_without_offset", Val::Text("2026-11-10T07:00:10.123".into())),
        ("display_updated_at", Val::Text("2026-11-10T05:00:10Z".into())),
        ("rows", Val::Int(9_007_199_254_740_993)),
        ("rows_sampled", Val::Int(2)),
        ("steps", Val::Int(2)),
        ("unfiltered_rows", Val::Int(9_007_199_254_740_993)),
        ("modification_counter", Val::Int(0)),
    ])
}
fn unavailable() -> Rec {
    replace(
        &row(),
        &[
            ("properties_available", Val::Int(0)),
            ("last_updated_without_offset", Val::Null),
            ("display_updated_at", Val::Null),
            ("rows", Val::Null),
            ("rows_sampled", Val::Null),
            ("steps", Val::Null),
            ("unfiltered_rows", Val::Null),
            ("modification_counter", Val::Null),
        ],
    )
}
#[tokio::test]
async fn shared_rows_preserve_exact_counts_native_time_and_missing_properties() {
    let value = decode(&row()).unwrap();
    assert_eq!(value.rows.as_deref(), Some("9007199254740993"));
    assert_eq!(value.modifications.as_deref(), Some("0"));
    assert_eq!(value.last_updated_without_offset.as_deref(), Some("2026-11-10T07:00:10.123"));
    assert_eq!(value.modifications_reset, culprit_core::ResetMarker::Unknown);
    let mut reader = FakeRead::new(vec![Ok(vec![row(), unavailable()])]);
    let app = application(&mut reader).await.unwrap();
    assert!(!app.properties_complete);
    assert_eq!(app.statistics.len(), 2, "a missing properties row cannot erase its catalog entry");
    assert!(app.statistics[1].rows.is_none());
    assert!(app.statistics[1].modifications.is_none());
    assert_eq!(app.statistics[0].last_updated.as_deref(), Some("2026-11-10T05:00:10Z"));
}
#[test]
fn absent_or_invalid_properties_do_not_become_zero_or_an_epoch() {
    let empty = decode(&unavailable()).unwrap();
    assert!(!empty.properties_available && empty.rows.is_none());
    let empty_blob = replace(&unavailable(), &[("properties_available", Val::Int(1))]);
    assert!(decode(&empty_blob).unwrap().last_updated_without_offset.is_none());
    for change in [
        ("modification_counter", Val::Int(-1)),
        ("auto_created", Val::Null),
        ("properties_available", Val::Int(0)),
        ("stats_id", Val::Int(0)),
        ("last_updated_without_offset", Val::Text("2026-11-10T07:00:10Z".into())),
    ] {
        assert!(decode(&replace(&row(), &[change])).is_err());
    }
}
#[tokio::test]
async fn oversized_and_failed_native_reads_never_fall_back_or_claim_complete_properties() {
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(application(&mut reader).await.is_err());
    assert_eq!(reader.calls.len(), 1);
    let mut reader = FakeRead::new(vec![Ok(vec![row(); APPLICATION_STATISTICS + 1])]);
    assert!(application(&mut reader).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}
