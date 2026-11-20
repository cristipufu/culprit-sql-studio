//! Infer the actual fixture futures without constructing a fixture, opening a client or polling work.
use super::*;
use std::{future::Future, mem::size_of};
const MAX_INLINE_FUTURE_BYTES: usize = 4096;
fn assert_future<A, F: Future>(phase: &str, _create: impl FnOnce(A) -> F) {
    let frame = size_of::<F>();
    let output = size_of::<F::Output>();
    eprintln!("suspect manufactured {phase}: future={frame},output={output}");
    assert!(frame <= MAX_INLINE_FUTURE_BYTES, "{phase} inlines{frame} bytes; retain the actual phase on the heap");
    assert!(output <= MAX_INLINE_FUTURE_BYTES, "{phase} returns{output} bytes through the panic/cleanup owner");
}
#[test]
fn actual_manufactured_history_future_types_keep_native_owners_and_outputs_off_the_poll_stack() {
    assert_future(
        "owned fixture body",
        |(fixture, cancellation, factory, originals): (
            &'static OwnedSourceFixture,
            &'static CollectionCancellation,
            &'static Factory,
            &'static mut OwnedCaptures,
        )| run(fixture, cancellation, factory, originals),
    );
    assert_future(
        "seed/capture/original verification",
        |(control, cancellation, fixture, factory, originals, seed): (
            &'static mut ReadSession,
            &'static CollectionCancellation,
            &'static OwnedSourceFixture,
            &'static Factory,
            &'static mut OwnedCaptures,
            &'static mut Option<Seeded>,
        )| work(control, cancellation, fixture, factory, originals, seed),
    );
    assert_future(
        "actual owned factory capture",
        |(originals, cancellation, factory): (
            &'static mut OwnedCaptures,
            &'static CollectionCancellation,
            &'static Factory,
        )| capture(originals, cancellation, factory),
    );
    assert_future(
        "actual native fixture query",
        |(reader, cancellation): (&'static mut ReadSession, &'static CollectionCancellation)| {
            query(reader, cancellation, SEED, vec![])
        },
    );
    assert_future(
        "exact native cleanup",
        |(reader, cancellation, seed): (&'static mut ReadSession, &'static CollectionCancellation, &'static Seeded)| {
            cleanup(reader, cancellation, seed, "unpolled-type-only")
        },
    );
}
