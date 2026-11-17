use super::*;

fn original_context() -> NativeReadContext {
    let original = super::super::test_focus(&page_wait(42, "PAGELATCH_EX", "7:1:4144")).participant;
    NativeReadContext {
        target: original.target,
        partition: original.partition,
        database_writable: original.database_writable,
        observed_from: original.original_observed_at.clone(),
        observed_until: original.original_observed_at,
    }
}

fn original_requests() -> Vec<ActivityRequestSelector> {
    selectors(&[page_wait(42, "PAGELATCH_EX", "7:1:4144")])
}

fn selectors(rows: &[Rec]) -> Vec<ActivityRequestSelector> {
    let mut requests = rows
        .iter()
        .map(|row| {
            let row = crate::activity::rows::record(row).unwrap().unwrap();
            ActivityRequestSelector { key: row.facts.key, statement_identity: row.facts.statement_identity.unwrap() }
        })
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| left.key.cmp(&right.key));
    requests
}

#[tokio::test]
async fn paired_population_reuses_all_native_phases_and_keeps_the_original_context_immutable() {
    let original = original_context();
    let retained = original.clone();
    let mut native = reader(successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]));
    let captured = native.capture_paired_page_wait_targets(&original, &original_requests()).await.unwrap();
    assert_eq!(original, retained);
    assert_eq!(captured.snapshot.observations.len(), 1);
    assert_eq!(native.query_count(), 10);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
    assert_eq!(native.io.closed, 1);
}

#[tokio::test]
async fn changed_original_owner_role_database_or_native_bounds_stops_before_population_or_page_headers() {
    for field in ["target", "member", "epoch", "role", "future-bounds"] {
        let mut original = original_context();
        match field {
            "target" => {
                let CollectionTarget::Database(database) = &mut original.target else { panic!() };
                database.database = "other".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut original.partition else {
                    panic!()
                };
                member.native.id = "different-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut original.partition else { panic!() };
                *epoch = Some("restarted-process".into());
            }
            "role" => original.database_writable = Some(false),
            _ => {
                original.observed_from = "2030-01-01T00:00:00Z".into();
                original.observed_until = original.observed_from.clone();
            }
        }
        let mut native = reader(vec![Ok(vec![context()])]);
        let error = native.capture_paired_page_wait_targets(&original, &original_requests()).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(native.query_count(), 1, "{field}");
        assert!(native.io.calls.iter().all(
            |call| !call.sql.contains("FROM sys.dm_os_waiting_tasks") && !call.sql.contains("sys.dm_db_page_info(")
        ));
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
        assert_eq!(native.io.closed, 1);
    }
}

#[tokio::test]
async fn unknown_original_does_no_native_work_and_unknown_current_role_does_not_read_a_population() {
    for field in ["owner", "role"] {
        let mut original = original_context();
        if field == "owner" {
            original.partition = SourcePartition::Target;
        } else {
            original.database_writable = None;
        }
        let mut native = reader(vec![]);
        assert!(native.capture_paired_page_wait_targets(&original, &original_requests()).await.is_err());
        assert_eq!(native.query_count(), 0);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
    let unknown = replace(&context(), &[("updateability", Val::Null)]);
    let mut native = reader(vec![Ok(vec![unknown])]);
    let error = native.capture_paired_page_wait_targets(&original_context(), &original_requests()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceUnavailable);
    assert_eq!(native.query_count(), 1);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

fn assert_no_deep_reads(reader: &ActivityReader<FakeRead>) {
    assert!(reader.io.calls.iter().all(|call| {
        !call.sql.contains("sys.dm_db_page_info(")
            && !call.sql.contains("sys.dm_db_index_operational_stats(")
            && !call.sql.contains("sys.dm_exec_sql_text(")
    }));
}

#[tokio::test]
async fn new_or_recycled_or_changed_requests_cannot_enrich_pages_for_the_original_frame() {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    for (field, value) in [
        ("session_id", Val::Int(99)),
        ("session_generation", Val::Text("2020-01-01T05:01:00".into())),
        ("request_id", Val::Int(2)),
        ("request_generation", Val::Text("2020-01-01T05:59:59".into())),
        ("sql_handle", Val::Text("EF".repeat(64))),
        ("statement_start", Val::Int(2)),
        ("statement_end", Val::Int(4)),
    ] {
        let current = replace(&original, &[(field, value)]);
        let mut native = reader(vec![Ok(vec![context()]), Ok(vec![current]), Ok(vec![context()])]);
        let capture = native
            .capture_paired_page_wait_targets(&original_context(), &selectors(std::slice::from_ref(&original)))
            .await
            .unwrap();
        assert!(capture.snapshot.observations.is_empty(), "{field}");
        assert_eq!(native.query_count(), 3, "{field}: only initial/population/final source proof");
        assert_no_deep_reads(&native);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn matching_opaque_tokens_still_need_the_current_native_database_scope() {
    let foreign = replace(&page_wait(42, "PAGELATCH_EX", "7:1:4144"), &[("request_database_id", Val::Int(8))]);
    let requests = selectors(std::slice::from_ref(&foreign));
    let mut native = reader(vec![Ok(vec![context()]), Ok(vec![foreign]), Ok(vec![context()])]);
    assert!(
        native
            .capture_paired_page_wait_targets(&original_context(), &requests)
            .await
            .unwrap()
            .snapshot
            .observations
            .is_empty()
    );
    assert_no_deep_reads(&native);
    assert_eq!(native.query_count(), 3);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn all_eligible_original_siblings_share_one_bounded_capture_without_enriching_a_new_actor() {
    let originals = vec![page_wait(42, "PAGELATCH_EX", "7:1:4144"), page_wait(43, "PAGELATCH_SH", "7:1:4145")];
    let requests = selectors(&originals);
    let mut replies = successful_replies(originals.clone());
    replies[1] = Ok(vec![page_wait(99, "PAGELATCH_EX", "7:1:9999"), originals[0].clone(), originals[1].clone()]);
    let mut native = reader(replies);
    let capture = native.capture_paired_page_wait_targets(&original_context(), &requests).await.unwrap();
    assert_eq!(
        capture.snapshot.observations.iter().map(|row| &row.request.key).collect::<BTreeSet<_>>(),
        requests.iter().map(|request| &request.key).collect()
    );
    assert_eq!(native.query_count(), 10, "the same admitted native phases, not a query per original actor");
    for call in &native.io.calls {
        for parameter in &call.parameters {
            if let Parameter::Text(value) = parameter {
                assert!(!value.contains("session=\"99\""));
                assert!(!value.contains("page=\"9999\""));
            }
        }
    }
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_broad_native_row_limit_remains_partial_after_unrelated_requests_are_filtered() {
    let current = (100..=164).map(|session| page_wait(session, "PAGELATCH_EX", "7:1:4144")).collect();
    let mut native = reader(vec![Ok(vec![context()]), Ok(current), Ok(vec![context()])]);
    let capture = native.capture_paired_page_wait_targets(&original_context(), &original_requests()).await.unwrap();
    assert!(capture.snapshot.observations.is_empty());
    assert!(capture.limited, "filtering a bounded prefix cannot establish full original-request visibility");
    assert_no_deep_reads(&native);
    assert_eq!(native.query_count(), 3);
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn an_original_actor_that_changes_after_population_is_rechecked_before_any_deep_read() {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    for (field, value) in
        [("checked_sql_handle", Val::Text("EF".repeat(64))), ("checked_wait_resource", Val::Text("7:1:4145".into()))]
    {
        let current = replace(&participant(0, &original), &[(field, value)]);
        let mut native =
            reader(vec![Ok(vec![context()]), Ok(vec![original.clone()]), Ok(vec![current]), Ok(vec![context()])]);
        let capture = native.capture_paired_page_wait_targets(&original_context(), &original_requests()).await.unwrap();
        assert_eq!(capture.snapshot.observations.len(), 1);
        assert!(capture.snapshot.observations[0].target.is_none());
        assert_no_deep_reads(&native);
        assert_eq!(native.query_count(), 4);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}
