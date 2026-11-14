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
    ]);
    Rec::of(&columns.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
#[tokio::test]
async fn retained_lookup_binds_selectors_and_requires_the_current_compilation_database() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![layout()]),
        Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
        Ok(vec![row()]),
        Ok(vec![crate::relational_inputs::tests::binding_row(&cached_statements::context(&row()).unwrap())]),
    ]);
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
    assert_eq!(reader.calls.len(), 4);
    assert!(reader.calls[3].sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
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

#[tokio::test]
async fn reused_or_missing_plan_binding_withholds_xml_and_preserves_the_earlier_native_context_and_counters() {
    let original = decode(&row(), 1024).unwrap();
    let binding = crate::relational_inputs::tests::binding_row(&original.context);
    for sql_handle in [Val::Text("0x9999".into()), Val::Null] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![row()]),
            Ok(vec![replace(&binding, &[("sql_handle", sql_handle)])]),
        ]);
        let observed = read(&mut reader, 7, Some(9), 1024).await.unwrap();
        assert_eq!(observed.setting_on, Some(true), "a changed binding is not evidence to turn statistics on");
        let observed = observed.plan.unwrap();
        assert!(matches!(observed.payload, Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        assert_eq!(observed.context, original.context);
        assert_eq!(
            observed.executions, original.executions,
            "module binding never replaces the earlier counter population"
        );
        assert_eq!(observed.last_start_without_offset, original.last_start_without_offset);
        assert_eq!(reader.calls.len(), 4);
    }
}

#[tokio::test]
async fn unknown_native_tokens_preserve_raw_counters_without_an_attribute_lookup() {
    for name in ["plan_handle", "sql_handle"] {
        let native = replace(&row(), &[(name, Val::Text("0x0000".into()))]);
        let mut reader =
            FakeRead::new(vec![Ok(vec![layout()]), Ok(vec![Rec::of(&[("value", Val::Int(1))])]), Ok(vec![native])]);
        let observed = read(&mut reader, 7, None, 1024).await.unwrap().plan.unwrap();
        assert_eq!(observed.executions, "2");
        assert!(matches!(observed.payload, Payload::Unavailable(PayloadUnavailable::NotCollected)));
        assert_eq!(reader.calls.len(), 3);
    }
}

#[tokio::test]
async fn terminal_final_proof_errors_do_not_become_missing_statistics_or_trigger_followup_reads() {
    for stop in [
        culprit_core::telemetry::CollectionStop::TimedOut,
        culprit_core::telemetry::CollectionStop::CancellationUnconfirmed,
        culprit_core::telemetry::CollectionStop::ByteLimit,
    ] {
        let mut reader = FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(vec![Rec::of(&[("value", Val::Int(1))])]),
            Ok(vec![row()]),
            Err(ReadError::Stopped(stop)),
        ]);
        let error = read(&mut reader, 7, None, 1024).await.err().unwrap();
        assert_eq!(culprit_core::telemetry::CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls.len(), 4);
    }
}
