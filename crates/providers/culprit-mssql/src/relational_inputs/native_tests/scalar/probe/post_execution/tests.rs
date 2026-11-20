use super::*;
use crate::{
    cached_statements,
    read::{Parameter, ReadError, ReadResult},
    relational_inputs::tests::binding_row,
    resources::test_support::{FakeRead, replace},
    test_support::owned_case,
    value::{Rec, Val},
};

fn original() -> (CachedStatementContext, i64) {
    let row = metadata::tests::row();
    (cached_statements::context(&row).unwrap(), crate::read::signed_integer(&row, "module_object_id").unwrap())
}
fn assert_original_absence(checked: &CheckedMetadata) {
    assert_eq!(checked.metadata.as_ref().err().unwrap().detail, CollectionStop::SourceUnavailable.error().detail);
}

#[tokio::test]
async fn verified_exact_original_starts_no_current_module_lookup() {
    let (original, object) = original();
    for before_execution in [false, true] {
        let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::row()]), Ok(vec![binding_row(&original)])]);
        let checked = if before_execution {
            read_before_execution(&mut reader, object, &original, 2).await
        } else {
            read(&mut reader, object, &original).await
        };
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(checked.metadata.as_ref().unwrap().context, original);
        assert!(checked.transition.is_none());
        assert_eq!(reader.queries(), 2);
    }
}

#[tokio::test]
async fn current_changed_incarnation_is_only_diagnostic_and_never_repairs_original_absence() {
    let (original, object) = original();
    let generation = original.identity.plan_generation.parse::<i64>().unwrap().checked_add(1).unwrap();
    let current = replace(&metadata::tests::row(), &[("plan_generation_num", Val::Int(generation))]);
    let current_context = cached_statements::context(&current).unwrap();
    assert_ne!(current_context.identity.plan_generation, original.identity.plan_generation);
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata::tests::missing_row()]),
        Ok(vec![current]),
        Ok(vec![binding_row(&current_context)]),
    ]);
    let checked = read(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&checked);
    let transition = checked.transition.as_ref().unwrap();
    assert_eq!((transition.exact_query_ordinal, transition.current_module_queries_after), (1, 3));
    let CurrentModule::Observed { comparison } = &transition.current_module else { panic!("current point recorded") };
    let flags = serde_json::to_value(comparison).unwrap();
    assert_eq!(flags["plan_generation_matches"], false);
    assert_eq!(flags["original_incarnation_matches"], false);
    assert_eq!(reader.queries(), 3);
}

#[tokio::test]
async fn completed_empty_current_module_is_distinct_from_an_unavailable_lookup() {
    let (original, object) = original();
    let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()]), Ok(vec![])]);
    let checked = read(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&checked);
    assert!(matches!(checked.transition.as_ref().unwrap().current_module, CurrentModule::NotFound));
    assert_eq!(reader.queries(), 2);
}

#[tokio::test]
async fn failed_or_unwitnessed_exact_reads_never_start_the_current_module_lookup() {
    let (original, object) = original();
    for response in [
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "private error"))),
        Err(ReadError::Stopped(CollectionStop::Cancelled)),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
        Ok(vec![]),
        Ok(vec![metadata::tests::row(), metadata::tests::row()]),
        Ok(vec![replace(&metadata::tests::missing_row(), &[("culprit_bind_database", Val::Int(0))])]),
    ] {
        let mut reader = FakeRead::new(vec![response]);
        let checked = read(&mut reader, object, &original).await;
        reader.close().await;
        assert!(checked.metadata.is_err());
        assert!(checked.transition.is_none());
        assert_eq!(reader.queries(), 1);
    }
}

#[tokio::test]
async fn secondary_module_failure_keeps_first_absence_and_original_shutdown_debt() {
    let (original, object) = original();
    for failure in [
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
        ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "PRIVATE_MODULE_ERROR_CANARY")),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()]), Err(failure)]);
        reader.shutdown = SourceShutdown::Unconfirmed;
        let checked = read(&mut reader, object, &original).await;
        assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
        assert_original_absence(&checked);
        let transition = checked.transition.as_ref().unwrap();
        assert!(matches!(
            transition.current_module,
            CurrentModule::Unavailable { stop: Some(CollectionStop::CancellationUnconfirmed), .. }
                | CurrentModule::Unavailable { code: ErrorCode::PermissionDenied, .. }
        ));
        assert!(!serde_json::to_string(transition).unwrap().contains("PRIVATE_MODULE_ERROR_CANARY"));
        assert_eq!(reader.queries(), 2);
    }
}

#[tokio::test]
async fn cancellation_after_witnessed_exact_absence_starts_no_current_module_query() {
    struct CancellingExactRead {
        reader: FakeRead,
        cancellation: CollectionCancellation,
    }
    #[async_trait::async_trait]
    impl NativeRead for CancellingExactRead {
        async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
            let result = self.reader.query(sql, parameters, limit).await;
            self.cancellation.cancel();
            result
        }
        fn queries(&self) -> u32 {
            self.reader.queries()
        }
        async fn close(&mut self) -> SourceShutdown {
            self.reader.close().await
        }
    }
    let (original, object) = original();
    for before_execution in [false, true] {
        let cancellation = CollectionCancellation::default();
        let mut reader = CancellingExactRead {
            reader: FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()])]),
            cancellation: cancellation.clone(),
        };
        let mut admitted = owned_case::AdmittedRead::new(&mut reader, &cancellation);
        let checked = if before_execution {
            read_before_execution(&mut admitted, object, &original, 3).await
        } else {
            read(&mut admitted, object, &original).await
        };
        assert_eq!(admitted.close().await, SourceShutdown::Quiescent);
        assert_original_absence(&checked);
        assert!(matches!(
            checked.transition.as_ref().unwrap().current_module,
            CurrentModule::Unavailable { stop: Some(CollectionStop::Cancelled), .. }
        ));
        assert_eq!(admitted.queries(), 1);
    }
}

#[test]
fn completed_plan_retention_redacts_values_without_rebinding_or_rewriting_native_work() {
    let directory = tempfile::tempdir().unwrap();
    let raw = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT N'PRIVATE_EXECUTION_CANARY'" QueryHash="0x1234567890123456"><QueryPlan><MemoryGrantInfo GrantedMemory="2048" IsMemoryGrantFeedbackAdjusted="Yes: Adjusting"/><ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(987654321)" ParameterRuntimeValue="(987654322)"/></ParameterList><RelOp NodeId="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1" ActualLogicalReads="777"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
    save_completed(directory.path(), &[raw.into()]).unwrap();
    let text = std::fs::read_to_string(directory.path().join("completed-execution-before-post-metadata.json")).unwrap();
    for private in ["PRIVATE_EXECUTION_CANARY", "987654321", "987654322"] {
        assert!(!text.contains(private), "{private}");
    }
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["post_execution_cache_binding_established"], false);
    assert_eq!(value["runtime_input_tuple_proof_retained"], false);
    let payload: DefinitionPayload = serde_json::from_value(value["redacted_native_plans"][0].clone()).unwrap();
    assert!(payload.is_valid());
    let DefinitionPayload::Available { text, .. } = payload else { panic!("valid redacted native body retained") };
    culprit_core::xml::read(&text, |doc| {
        let doc = doc.unwrap();
        assert!(doc.descendants().any(|node| node.attribute("ActualLogicalReads") == Some("777")));
        assert!(
            doc.descendants().any(|node| node.attribute("IsMemoryGrantFeedbackAdjusted") == Some("Yes: Adjusting"))
        );
    });
}

#[test]
fn failed_redaction_keeps_the_original_unavailable_reason_without_a_raw_body_fallback() {
    let directory = tempfile::tempdir().unwrap();
    save_completed(directory.path(), &["<ShowPlanXML PRIVATE_BROKEN_CANARY".into()]).unwrap();
    let text = std::fs::read_to_string(directory.path().join("completed-execution-before-post-metadata.json")).unwrap();
    assert!(!text.contains("PRIVATE_BROKEN_CANARY"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let payload: DefinitionPayload = serde_json::from_value(value["redacted_native_plans"][0].clone()).unwrap();
    assert_eq!(payload, DefinitionPayload::Unavailable { reason: PayloadUnavailable::RedactionFailed });
}

#[tokio::test]
async fn pre_execution_completed_absence_retains_native_predicates_and_full_context_without_repair() {
    let (original, object) = original();
    let current = metadata::tests::predicate_row(metadata::tests::row());
    let mut reader = FakeRead::new(vec![
        Ok(vec![metadata::tests::missing_row()]),
        Ok(vec![current]),
        Ok(vec![binding_row(&original)]),
    ]);
    let checked = read_before_execution(&mut reader, object, &original, 3).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&checked);
    let transition = checked.transition.as_ref().unwrap();
    assert_eq!((transition.exact_query_ordinal, transition.current_module_queries_after), (1, 3));
    assert_eq!(transition.module_followup_dispatch_cap, 2);
    let CurrentModule::PredicatesObserved { point } = &transition.current_module else {
        panic!("native diagnostic point")
    };
    let retained = serde_json::to_value(point).unwrap();
    assert_eq!(retained["context"], serde_json::to_value(&original).unwrap());
    assert!(retained["native_predicates"].as_object().unwrap().values().all(|value| value.as_bool() == Some(true)));
    assert_eq!(retained["original_binding_verified"], true);
    assert_eq!(reader.queries(), 3);
}

#[tokio::test]
async fn pre_execution_followup_reserves_both_slots_and_never_follows_an_unavailable_exact_read() {
    let (original, object) = original();
    let mut limited = FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()])]);
    let checked = read_before_execution(&mut limited, object, &original, 2).await;
    assert_original_absence(&checked);
    assert!(matches!(
        checked.transition.as_ref().unwrap().current_module,
        CurrentModule::Unavailable { stop: Some(CollectionStop::QueryLimit), .. }
    ));
    assert_eq!(limited.queries(), 1);
    for response in [
        Err(ReadError::Stopped(CollectionStop::Cancelled)),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "PRIVATE_EXACT_ERROR"))),
        Ok(vec![]),
        Ok(vec![metadata::tests::row(), metadata::tests::row()]),
        Ok(vec![replace(&metadata::tests::missing_row(), &[("culprit_bind_database", Val::Null)])]),
    ] {
        let mut reader = FakeRead::new(vec![response]);
        let checked = read_before_execution(&mut reader, object, &original, 3).await;
        assert!(checked.metadata.is_err());
        assert!(checked.transition.is_none());
        assert_eq!(reader.queries(), 1);
    }
}

#[tokio::test]
async fn pre_execution_secondary_fence_or_ambiguous_point_preserves_first_absence_and_shutdown_debt() {
    let (original, object) = original();
    for response in [
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
        Err(ReadError::Source(culprit_core::Error::new(ErrorCode::PermissionDenied, "PRIVATE_CURRENT_ERROR"))),
        Ok(vec![
            metadata::tests::predicate_row(metadata::tests::row()),
            metadata::tests::predicate_row(metadata::tests::row()),
        ]),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()]), response]);
        reader.shutdown = SourceShutdown::Unconfirmed;
        let checked = read_before_execution(&mut reader, object, &original, 3).await;
        assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
        assert_original_absence(&checked);
        let transition = checked.transition.as_ref().unwrap();
        assert!(matches!(transition.current_module, CurrentModule::Unavailable { .. }));
        assert!(!serde_json::to_string(transition).unwrap().contains("PRIVATE_CURRENT_ERROR"));
        assert_eq!(reader.queries(), 2);
    }
}
