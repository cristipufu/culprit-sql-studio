//! The existing proof runs in one local task with the original fixture; no body borrows its controller.
use super::{Case, Fixture};
use crate::test_support::owned_case::run_owned_body;
use culprit_core::Result;
use futures_util::future::LocalBoxFuture;

pub(super) async fn run(case: Case, fixture: Box<Fixture>) -> (Box<Fixture>, std::thread::Result<Result<()>>) {
    run_owned_body(fixture, move |fixture| select(case, fixture)).await
}

// Selecting a branch constructs only its existing boxed future. An async match here would inline every
// branch's poll temporaries while a native connection is being opened by the selected proof.
fn select(case: Case, fixture: &mut Fixture) -> LocalBoxFuture<'_, Result<()>> {
    use crate::relational_inputs::native_tests::scalar::retained::grant_pressure;
    match case {
        Case::Reservations => Box::pin(super::super::reservations::prove(fixture)),
        Case::DrainOutput => Box::pin(super::super::drain::prove(fixture)),
        Case::OriginalScalarSourcePhases => Box::pin(grant_pressure::source_phase_control(fixture)),
        Case::OriginalScalarCacheControl => Box::pin(grant_pressure::cache_without_collection(fixture)),
        Case::OriginalScalarGrant => Box::pin(grant_pressure::prove(fixture)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, mem::size_of};

    const MAX_INLINE_BODY_BYTES: usize = 4 * 1024;

    fn assert_storage<A, F: Future>(name: &str, _create: impl FnOnce(A) -> F) {
        let future = size_of::<F>();
        let output = size_of::<F::Output>();
        eprintln!("owned grant {name}: future={future} bytes; output={output} bytes");
        assert!(future <= MAX_INLINE_BODY_BYTES, "{name} stores {future} bytes inline");
        assert!(output <= MAX_INLINE_BODY_BYTES, "{name} returns {output} bytes inline");
    }

    #[test]
    fn actual_native_body_task_and_concrete_owner_keep_storage_bounded() {
        // Infer the real task and inner owner types without constructing/polling native actors. These are
        // stored-future bounds, separate from the scheduling controls and original native acceptance.
        assert_storage("body task", |fixture: Box<Fixture>| run(Case::OriginalScalarGrant, fixture));
        assert_storage("concrete original body owner", |fixture: Box<Fixture>| {
            run_owned_body(fixture, |fixture| select(Case::OriginalScalarGrant, fixture))
        });
    }
}
