use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
fn layout() -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Int(1)),
        ("last_actual", Val::Int(1)),
        ("live_xml", Val::Int(1)),
        ("profiles", Val::Int(1)),
    ])
}
fn row() -> Rec {
    let mut columns = crate::grants::tests::row()
        .columns()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect::<Vec<_>>();
    columns.extend([
        (String::from("plan_bytes"), Val::Int(2)),
        ("query_plan".into(), Val::Text("x".into())),
        ("encrypted".into(), Val::Bool(false)),
        ("last_start_without_offset".into(), Val::Text("2020-01-01T08:00:00.003".into())),
        ("last_start_utc_estimate".into(), Val::Text("2020-01-01T06:00:00Z".into())),
    ]);
    Rec::of(&columns.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
#[tokio::test]
async fn retained_lookup_binds_selectors_and_requires_the_current_compilation_database() {
    let mut reader =
        FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![Rec::of(&[("value", Val::Int(1))])]), Ok(vec![row()])]);
    let result = read(&mut reader, 7, Some(9), 1024).await.unwrap();
    let plan = result.plan.unwrap();
    assert!(plan.payload.text().is_some());
    assert_eq!(plan.last_start_without_offset.as_deref(), Some("2020-01-01T08:00:00.0030000"));
    let call = &reader.calls[2];
    assert!(call.sql.contains(cached_statements::DATABASE_PREDICATE));
    assert!(call.sql.contains("q.query_id=@P1"));
    assert!(call.sql.contains("p.plan_id=@P2"));
    assert!(matches!(call.parameters[0], Parameter::Integer(7)));
    assert!(matches!(call.parameters[1], Parameter::Integer(9)));
}
#[tokio::test]
async fn failed_setting_reads_never_continue_with_a_plan_lookup() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout()]),
        Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::TimedOut)),
    ]);
    let error = read(&mut reader, 7, None, 1024).await.err().unwrap();
    assert_eq!(
        culprit_core::telemetry::CollectionStop::from_error(&error),
        culprit_core::telemetry::CollectionStop::TimedOut
    );
    assert_eq!(reader.calls.len(), 2);
}
#[tokio::test]
async fn oversized_retained_xml_is_unavailable_without_truncation() {
    let oversized = replace(&row(), &[("plan_bytes", Val::Int(2048)), ("query_plan", Val::Null)]);
    let mut reader =
        FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![Rec::of(&[("value", Val::Int(1))])]), Ok(vec![oversized])]);
    let result = read(&mut reader, 7, None, 1024).await.unwrap();
    assert!(matches!(
        result.plan.unwrap().payload,
        Payload::Unavailable(culprit_core::telemetry::PayloadUnavailable::Oversized)
    ));
}
