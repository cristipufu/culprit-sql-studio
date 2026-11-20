//! Measure the actual scalar phase types without creating a client or polling fixture work.
use super::*;
use std::{future::Future, mem::size_of};

const MAX_INLINE_FUTURE_BYTES: usize = 4096;

fn assert_future_bound<A, F: Future>(phase: &str, _create: impl FnOnce(A) -> F) {
    let bytes = size_of::<F>();
    assert!(bytes <= MAX_INLINE_FUTURE_BYTES, "{phase} inlines {bytes} bytes; retain the owned phase on the heap");
}

#[test]
fn scalar_execution_phases_keep_native_results_off_the_poll_stack() {
    // Only infer the real future types. A native Probe cannot be manufactured just to inspect its footprint.
    assert_future_bound("redacted execution", |probe: &'static mut Probe| probe.record(1, CacheExpectation::New));
    assert_future_bound("native execution", |probe: &'static mut Probe| probe.record_native(1, CacheExpectation::New));
    assert_future_bound(
        "coordinated original execution",
        |(probe, submitting): (&'static mut Probe, &'static tokio::sync::Notify)| {
            probe.record_native_notifying(1, CacheExpectation::New, submitting, None)
        },
    );
    assert_future_bound(
        "memory phase diagnostic",
        |(probe, original, observer, directory): (
            &'static Probe,
            &'static CachedStatementContext,
            Arc<Pool>,
            &'static std::path::Path,
        )| { probe.observe_memory_source_phases(original, observer, directory) },
    );
    assert_future_bound(
        "conditional native predicate point",
        |(reader, original): (
            &'static mut crate::test_support::owned_case::AdmittedRead<'static, ReadSession>,
            &'static CachedStatementContext,
        )| { metadata::compare_predicates(reader, 1001, original) },
    );
    assert_future_bound(
        "pre-execution exact and conditional predicate comparison",
        |(reader, original): (
            &'static mut crate::test_support::owned_case::AdmittedRead<'static, ReadSession>,
            &'static CachedStatementContext,
        )| {
            post_execution::read_before_execution(
                reader,
                1001,
                original,
                super::super::super::expressions::reader_budget().max_queries,
            )
        },
    );
    assert_future_bound(
        "post-execution exact and diagnostic module selection",
        |(reader, original): (
            &'static mut crate::test_support::owned_case::AdmittedRead<'static, ReadSession>,
            &'static CachedStatementContext,
        )| { post_execution::read(reader, 1001, original) },
    );
}
