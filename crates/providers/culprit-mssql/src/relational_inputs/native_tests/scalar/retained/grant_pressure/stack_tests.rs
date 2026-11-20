//! Measure real owned phase/response types without constructing or polling any native fixture.
use super::*;
use std::{future::Future, mem::size_of};

const MAX_INLINE_PHASE_BYTES: usize = 4096;

fn assert_owned_phase<A, F: Future>(name: &str, _create: impl FnOnce(A) -> F) {
    let frame = size_of::<F>();
    let output = size_of::<F::Output>();
    eprintln!("owned scalar grant {name}: future={frame} bytes; response={output} bytes");
    assert!(frame <= MAX_INLINE_PHASE_BYTES, "{name} inlines {frame} bytes; retain the phase on the heap");
    assert!(output <= MAX_INLINE_PHASE_BYTES, "{name} returns {output} inline bytes through panic wrappers");
}

#[test]
fn actual_episode_and_waiter_keep_owned_native_results_off_the_poll_stack() {
    // Only infer the actual concrete future/output types. These closures are never invoked.
    assert_owned_phase("episode", |(fixture, probe): (&'static mut Fixture, &'static mut Probe)| {
        capture_episode(fixture, probe)
    });
    assert_owned_phase(
        "concrete native waiter observation body",
        |(fixture, owner, submitting, actor, statement, holder, source): (
            &'static mut Fixture,
            &'static mut OwnedCaptures,
            &'static Notify,
            &'static probe::Actor,
            &'static CachedStatementContext,
            &'static ExecutionGrantObservation,
            &'static CollectionDescription,
        )| { capture_waiter(fixture, owner, submitting, actor, statement, holder, source) },
    );
    assert_owned_phase(
        "waiter capture and original-holder release",
        |(fixture, owner, submitting, actor, statement, holder, source): (
            &'static mut Fixture,
            &'static mut OwnedCaptures,
            &'static Notify,
            &'static probe::Actor,
            &'static CachedStatementContext,
            &'static ExecutionGrantObservation,
            &'static CollectionDescription,
        )| { capture_waiter_and_release(fixture, owner, submitting, actor, statement, holder, source) },
    );
}
