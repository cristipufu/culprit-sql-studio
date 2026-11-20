use super::*;
use crate::{
    cached_statements,
    read::{Parameter, ReadError, ReadResult},
    relational_inputs::tests::binding_row,
    resources::test_support::{FakeRead, replace},
    test_support::owned_case,
    value::{Rec, Val},
};
use culprit_collector::capture::CollectionCancellation;

fn original() -> (CachedStatementContext, i64) {
    let row = metadata::tests::row();
    (cached_statements::context(&row).unwrap(), crate::read::signed_integer(&row, "module_object_id").unwrap())
}

fn assert_original_absence(observed: &ObservedSelection) {
    assert_eq!(
        observed.comparison.as_ref().unwrap_err().detail,
        CollectionStop::SourceUnavailable.error().detail,
        "the original module-only assertion must fail independently of the exact check"
    );
}

#[tokio::test]
async fn present_module_keeps_original_comparison_and_starts_no_exact_diagnostic() {
    let (original, object) = original();
    let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::row()]), Ok(vec![binding_row(&original)])]);
    let observed = observe(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert!(observed.absence.is_none());
    let flags = serde_json::to_value(observed.comparison.unwrap()).unwrap();
    assert!(flags.as_object().unwrap().values().all(|value| value.as_bool() == Some(true)));
    assert_eq!(reader.queries(), 2);
    assert_eq!(reader.calls[0].parameters.len(), 2);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn completed_module_absence_can_record_exact_presence_but_never_becomes_success() {
    let (original, object) = original();
    let mut reader =
        FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::row()]), Ok(vec![binding_row(&original)])]);
    let observed = observe(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&observed);
    let diagnostic = observed.absence.unwrap();
    assert!(matches!(diagnostic.exact_original, Ok(ExactCachePresence::Matched)));
    assert!(diagnostic.direct_binding_after_exact_absence.is_none());
    assert_eq!((diagnostic.module_query_ordinal, diagnostic.exact_queries_after), (1, 3));
    assert_eq!(reader.queries(), super::super::DIAGNOSTIC_QUERY_CAP);
    assert_eq!(reader.calls[0].parameters.len(), 2);
    assert_eq!(reader.calls[1].parameters.len(), 8);
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn exact_absence_requires_the_existing_complete_native_database_and_parameter_witness() {
    let (original, object) = original();
    let absent_binding = replace(&binding_row(&original), &[("sql_handle", Val::Null)]);
    let mut reader =
        FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::missing_row()]), Ok(vec![absent_binding])]);
    let observed = observe(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&observed);
    let absence = observed.absence.unwrap();
    assert!(matches!(absence.exact_original, Ok(ExactCachePresence::NotFound)));
    let binding = absence.direct_binding_after_exact_absence.unwrap();
    assert!(matches!(binding.matching_plan_sql_binding_visible, Ok(false)));
    assert_eq!((absence.exact_queries_after, binding.queries_before, binding.queries_after), (2, 2, 3));
    assert_eq!(reader.queries(), super::super::DIAGNOSTIC_QUERY_CAP);
}

#[tokio::test]
async fn witnessed_statement_statistics_absence_can_keep_the_same_plan_sql_binding_visible() {
    let (original, object) = original();
    let mut reader =
        FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::missing_row()]), Ok(vec![binding_row(&original)])])
            .with_query_limit(super::super::DIAGNOSTIC_QUERY_CAP);
    let observed = observe(&mut reader, object, &original).await;

    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&observed);
    let absence = observed.absence.unwrap();
    assert!(matches!(absence.exact_original, Ok(ExactCachePresence::NotFound)));
    let binding = absence.direct_binding_after_exact_absence.unwrap();
    assert!(matches!(binding.matching_plan_sql_binding_visible, Ok(true)));
    assert_eq!((absence.exact_queries_after, binding.queries_before, binding.queries_after), (2, 2, 3));
    assert_eq!(reader.queries(), super::super::DIAGNOSTIC_QUERY_CAP);
    assert_eq!(reader.calls[2].sql, crate::relational_inputs::binding_check_sql());
    assert_eq!(reader.calls[2].parameters.len(), 1);
    assert_eq!(reader.calls[2].limit, 2);
    assert!(!reader.calls[2].sql.contains("sys.dm_exec_query_stats"));
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn absent_or_conflicting_direct_binding_remains_visibility_evidence_without_original_incarnation() {
    let (original, object) = original();
    for sql_handle in [Val::Null, Val::Text("0x9999".into())] {
        let binding = replace(&binding_row(&original), &[("sql_handle", sql_handle)]);
        let mut reader = FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::missing_row()]), Ok(vec![binding])]);
        let observed = observe(&mut reader, object, &original).await;

        reader.close().await;
        assert_original_absence(&observed);
        let binding = observed.absence.unwrap().direct_binding_after_exact_absence.unwrap();
        assert!(matches!(binding.matching_plan_sql_binding_visible, Ok(false)));
        let encoded = serde_json::to_string(&binding).unwrap();
        assert!(!encoded.contains(&original.identity.plan_handle));
        assert!(!encoded.contains(&original.identity.sql_handle));
        assert!(!encoded.contains("evicted") && !encoded.contains("replacement"));
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn direct_binding_errors_cannot_replace_original_absence_or_clear_shutdown_debt() {
    let (original, object) = original();
    for failure in [
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
        ReadError::Stopped(CollectionStop::TimedOut),
        ReadError::Source(Error::new(ErrorCode::PermissionDenied, "private-direct-binding-error")),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::missing_row()]), Err(failure)]);
        reader.shutdown = SourceShutdown::Unconfirmed;
        let observed = observe(&mut reader, object, &original).await;

        assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
        assert_original_absence(&observed);
        let binding = observed.absence.unwrap().direct_binding_after_exact_absence.unwrap();
        assert!(binding.matching_plan_sql_binding_visible.is_err());
        assert!(!serde_json::to_string(&binding).unwrap().contains("private-direct-binding-error"));
        assert_eq!((binding.queries_before, binding.queries_after), (2, 3));
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn native_module_errors_or_stops_do_not_admit_the_conditional_exact_query() {
    let (original, object) = original();
    for failure in [
        ReadError::Source(Error::new(ErrorCode::PermissionDenied, "private error")),
        ReadError::Stopped(CollectionStop::Cancelled),
        ReadError::Stopped(CollectionStop::TimedOut),
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
    ] {
        let mut reader = FakeRead::new(vec![Err(failure)]);
        let observed = observe(&mut reader, object, &original).await;
        reader.close().await;
        assert!(observed.comparison.is_err());
        assert!(observed.absence.is_none());
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.closed, 1);
    }
}

#[tokio::test]
async fn ambiguous_or_partial_module_headers_are_errors_not_completed_empty_population() {
    let (original, object) = original();
    for rows in [
        vec![metadata::tests::row(), metadata::tests::row()],
        vec![replace(&metadata::tests::row(), &[("text_prefix", Val::Null)])],
        vec![replace(&metadata::tests::row(), &[("encrypted", Val::Bool(true))])],
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        let observed = observe(&mut reader, object, &original).await;
        reader.close().await;
        assert!(observed.comparison.is_err());
        assert!(observed.absence.is_none());
        assert_eq!(reader.queries(), 1);
    }
}

#[tokio::test]
async fn missing_or_false_exact_database_witness_is_unknown_not_known_cache_absence() {
    let (original, object) = original();
    for value in [Val::Null, Val::Int(0), Val::Int(2)] {
        let exact = replace(&metadata::tests::missing_row(), &[("culprit_bind_database", value)]);
        let mut reader = FakeRead::new(vec![Ok(vec![]), Ok(vec![exact])]);
        let observed = observe(&mut reader, object, &original).await;
        reader.close().await;
        assert_original_absence(&observed);
        assert!(observed.absence.unwrap().exact_original.is_err());
        assert_eq!(reader.queries(), 2);
    }
}

#[tokio::test]
async fn secondary_native_error_and_shutdown_debt_cannot_replace_the_original_module_error() {
    let (original, object) = original();
    for failure in [
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
        ReadError::Source(Error::new(ErrorCode::PermissionDenied, "private-native-error-canary")),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![]), Err(failure)]);
        reader.shutdown = SourceShutdown::Unconfirmed;
        let observed = observe(&mut reader, object, &original).await;
        assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
        assert_original_absence(&observed);
        let diagnostic = observed.absence.unwrap();
        assert!(!serde_json::to_string(&diagnostic).unwrap().contains("private-native-error-canary"));
        let Err(error) = diagnostic.exact_original else { panic!("native exact check remains unavailable") };
        assert!(
            error.stop == Some(CollectionStop::CancellationUnconfirmed) || error.code == ErrorCode::PermissionDenied
        );
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.closed, 1);
    }
}

struct CancellingRead {
    reader: FakeRead,
    cancellation: CollectionCancellation,
    cancel_after: u32,
}
#[async_trait::async_trait]
impl NativeRead for CancellingRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        let result = self.reader.query(sql, parameters, limit).await;
        if self.reader.queries() == self.cancel_after {
            self.cancellation.cancel();
        }
        result
    }
    fn queries(&self) -> u32 {
        self.reader.queries()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.reader.close().await
    }
}

#[tokio::test]
async fn cancellation_after_completed_module_absence_starts_no_exact_query() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut reader =
        CancellingRead { reader: FakeRead::new(vec![Ok(vec![])]), cancellation: cancellation.clone(), cancel_after: 1 };
    let mut admitted = owned_case::AdmittedRead::new(&mut reader, &cancellation);
    let observed = observe(&mut admitted, object, &original).await;
    assert_eq!(admitted.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&observed);
    let Err(error) = observed.absence.unwrap().exact_original else {
        panic!("cancelled exact check stays unavailable")
    };
    assert_eq!(error.stop, Some(CollectionStop::Cancelled));
    assert_eq!(admitted.queries(), 1);
}

#[tokio::test]
async fn cancellation_after_witnessed_exact_absence_starts_no_direct_binding_query() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut reader = CancellingRead {
        reader: FakeRead::new(vec![Ok(vec![]), Ok(vec![metadata::tests::missing_row()])]),
        cancellation: cancellation.clone(),
        cancel_after: 2,
    };
    let mut admitted = owned_case::AdmittedRead::new(&mut reader, &cancellation);
    let observed = observe(&mut admitted, object, &original).await;
    assert_eq!(admitted.close().await, SourceShutdown::Quiescent);
    assert_original_absence(&observed);
    let binding = observed.absence.unwrap().direct_binding_after_exact_absence.unwrap();
    let Err(error) = binding.matching_plan_sql_binding_visible else { panic!("cancelled binding remains unavailable") };
    assert_eq!(error.stop, Some(CollectionStop::Cancelled));
    assert_eq!((binding.queries_before, binding.queries_after), (2, 2));
    assert_eq!(admitted.queries(), 2);
}

#[test]
fn actual_cache_observation_owns_the_nested_native_selection_future_on_the_heap() {
    use super::super::super::Probe;
    use crate::read::ReadSession;
    use std::{future::Future, mem::size_of, path::Path};

    fn frame<A, F: Future>(_create: impl FnOnce(A) -> F) -> (usize, usize) {
        (size_of::<F>(), size_of::<F::Output>())
    }
    // This is the actual admitted native ReadSession specialization, not FakeRead or a copied future body.
    // Only infer types; no closure is invoked, client constructed or database/configuration read.
    type OriginalReader = owned_case::AdmittedRead<'static, ReadSession>;
    let module = frame(|reader: &'static mut OriginalReader| metadata::find(reader, 1001, None));
    let exact = frame(|(reader, original): (&'static mut OriginalReader, &'static CachedStatementContext)| {
        metadata::find(reader, 1001, Some(original))
    });
    let selected = frame(|(reader, original): (&'static mut OriginalReader, &'static CachedStatementContext)| {
        observe(reader, 1001, original)
    });
    let selected_original =
        frame(|(reader, original): (&'static mut OriginalReader, &'static CachedStatementContext)| {
            observe_original(reader, 1001, original)
        });
    let owner =
        frame(|(probe, original, directory): (&'static Probe, &'static CachedStatementContext, &'static Path)| {
            probe.observe_current_cache_context(original, directory, "unpolled-native-frame")
        });
    let original_owner =
        frame(|(probe, original, directory): (&'static Probe, &'static CachedStatementContext, &'static Path)| {
            probe.observe_original_cache_context(original, directory, "unpolled-native-original")
        });
    eprintln!(
        "actual native cache future/response bytes: module={module:?}; exact={exact:?}; selection={selected:?}; original_selection={selected_original:?}; owner={owner:?}; original_owner={original_owner:?}"
    );
    const MAX_INLINE_OWNER_BYTES: usize = 4096;
    // This asserts the concrete async selection and its response, not its erased heap-owned caller.
    assert!(selected_original.0 <= MAX_INLINE_OWNER_BYTES, "original selection inlines {} bytes", selected_original.0);
    assert!(
        selected_original.1 <= MAX_INLINE_OWNER_BYTES,
        "original selection returns {} inline bytes",
        selected_original.1
    );
    assert!(owner.0 <= MAX_INLINE_OWNER_BYTES, "cache observation inlines {} bytes of nested native state", owner.0);
    assert!(owner.1 <= MAX_INLINE_OWNER_BYTES, "cache observation returns {} inline bytes", owner.1);
    assert!(
        original_owner.0 <= MAX_INLINE_OWNER_BYTES,
        "original cache observation inlines {} bytes",
        original_owner.0
    );
    assert!(
        original_owner.1 <= MAX_INLINE_OWNER_BYTES,
        "original cache observation returns {} inline bytes",
        original_owner.1
    );
}

#[tokio::test]
async fn pre_source_original_selection_uses_the_exact_native_identity_and_existing_binding_within_two_queries() {
    let (original, object) = original();
    let mut reader = FakeRead::new(vec![Ok(vec![metadata::tests::row()]), Ok(vec![binding_row(&original)])])
        .with_query_limit(super::super::EXACT_PHASE_QUERY_CAP);
    let observed = observe_original(&mut reader, object, &original).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);

    assert!(observed.absence.is_none());
    observed.comparison.unwrap().require_original_match().unwrap();
    assert_eq!(reader.calls.len(), 2);
    assert_eq!(reader.calls[0].parameters.len(), 8);
    for predicate in [
        "qs.plan_handle=CONVERT(varbinary(64),@P3,1)",
        "qs.sql_handle=CONVERT(varbinary(64),@P4,1)",
        "qs.statement_start_offset=@P5",
        "qs.statement_end_offset=@P6",
        "qs.plan_generation_num=@P7",
        "CAST(pa.value AS int)=@P8",
    ] {
        assert!(reader.calls[0].sql.contains(predicate));
    }
    assert_eq!(reader.calls[1].sql, crate::relational_inputs::binding_check_sql());
    assert_eq!(reader.closed, 1);
}

#[tokio::test]
async fn pre_source_absent_changed_unverified_or_cancelled_original_never_accepts_a_current_module() {
    let (original, object) = original();
    let changed = replace(&metadata::tests::row(), &[("plan_generation_num", Val::Int(9))]);
    let unverified = replace(&binding_row(&original), &[("sql_handle", Val::Text("0x9999".into()))]);
    for (responses, stop, queries) in [
        (vec![Ok(vec![metadata::tests::missing_row()])], CollectionStop::SourceUnavailable, 1),
        (vec![Ok(vec![changed])], CollectionStop::SourceChanged, 1),
        (vec![Ok(vec![metadata::tests::row()]), Ok(vec![unverified])], CollectionStop::SourceChanged, 2),
        (vec![Err(ReadError::Stopped(CollectionStop::Cancelled))], CollectionStop::Cancelled, 1),
        (
            vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))],
            CollectionStop::CancellationUnconfirmed,
            1,
        ),
    ] {
        let mut reader = FakeRead::new(responses).with_query_limit(super::super::EXACT_PHASE_QUERY_CAP);
        let observed = observe_original(&mut reader, object, &original).await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        assert_eq!(CollectionStop::from_error(observed.comparison.as_ref().unwrap_err()), stop);
        assert!(observed.absence.is_none(), "no module diagnostic or replacement point is admitted");

        assert_eq!(reader.queries(), queries);
        assert_eq!(reader.calls[0].parameters.len(), 8);
        assert_eq!(reader.closed, 1);
    }
}
