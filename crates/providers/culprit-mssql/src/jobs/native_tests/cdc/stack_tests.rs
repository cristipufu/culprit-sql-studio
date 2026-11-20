//! Inspect real async entry types without constructing an admitted owner or polling database work.
use super::*;
use std::{future::Future, mem::size_of};

// Leave the default test stack for native decoding/poll frames; fixture state and I/O phases live on the heap.
const MAX_INLINE_FUTURE_BYTES: usize = 4 * 1024;

fn future_bytes<A, F: Future>(phase: &str, _create: impl FnOnce(A) -> F) -> usize {
    let bytes = size_of::<F>();
    eprintln!("CDC fixture {phase} future = {bytes} bytes");
    bytes
}

#[test]
fn native_fixture_phase_futures_and_client_transport_stay_off_the_default_test_stack() {
    // These factories supply types only. None is invoked, so no credentials/environment, connection,
    // directory, nonce admission, source query or fixture SQL is needed to inspect the actual footprints.
    let mut phases = vec![
        ("owned case", future_bytes("owned case", |()| run_owned())),
        (
            "control connection and nonce verification",
            future_bytes("control connection and nonce verification", |fixture: &'static mut Fixture| {
                fixture.open_control()
            }),
        ),
        (
            "native query and drain",
            future_bytes("native query and drain", |fixture: &'static mut Fixture| {
                fixture.query("size-only", "unused")
            }),
        ),
        (
            "first-table setup query",
            future_bytes("first-table setup query", |fixture: &'static mut Fixture| {
                fixture.query_with_timeout("size-only", FIRST_TABLE_SETUP_TIMEOUT, "unused")
            }),
        ),
        (
            "owned capture job settlement",
            future_bytes("owned capture job settlement", |fixture: &'static mut Fixture| fixture.disable_capture_job()),
        ),
        (
            "original source capture",
            future_bytes(
                "original source capture",
                |(fixture, captures, cancellation): (
                    &'static mut Fixture,
                    &'static mut OwnedCaptures,
                    &'static CollectionCancellation,
                )| { fixture.capture(captures, "size-only", cancellation) },
            ),
        ),
        (
            "strict native phases",
            future_bytes(
                "strict native phases",
                |(fixture, cancellation): (&'static mut Fixture, &'static CollectionCancellation)| {
                    fixture.exercise(cancellation)
                },
            ),
        ),
        (
            "original control retirement",
            future_bytes("original control retirement", |fixture: &'static mut Fixture| fixture.retire()),
        ),
        ("cleanup", future_bytes("cleanup", |fixture: &'static mut Fixture| fixture.cleanup())),
        (
            "control client result",
            future_bytes("control client result", |control: &'static mut Control| {
                control.run("size-only", std::future::pending::<std::result::Result<Box<MsClient>, ControlFailure>>())
            }),
        ),
        (
            "physical close control",
            future_bytes("physical close control", |(control, client): (&'static mut Control, Box<MsClient>)| {
                control.retire(Box::pin((*client).close()))
            }),
        ),
    ];
    phases.extend(capture_job::phase_future_sizes());
    // Print every actual footprint before asserting, so an oversized first phase cannot hide the rest.
    for (phase, bytes) in phases {
        assert!(bytes <= MAX_INLINE_FUTURE_BYTES, "{phase} future inlines {bytes} bytes; box the actual owned phase");
    }
    assert!(size_of::<Fixture>() <= MAX_INLINE_FUTURE_BYTES, "retained native fixture state must remain bounded");
    assert!(
        size_of::<Result<Box<MsClient>>>() <= MAX_INLINE_FUTURE_BYTES,
        "the completed client carrier must remain bounded as it crosses await/control boundaries"
    );
    assert!(size_of::<Captured>() <= MAX_INLINE_FUTURE_BYTES, "original captured results must remain bounded");
}
