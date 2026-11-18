//! Inspect the actual async entry types without opening a connection or polling an owned workload.
use super::*;
use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use culprit_core::telemetry::{CollectionSelection, SnapshotDataset};
use std::{future::Future, mem::size_of, sync::Arc};

const MAX_INLINE_FUTURE_BYTES: usize = 4096;

fn assert_future_bound<A, F: Future>(phase: &str, _create: impl FnOnce(A) -> F) {
    assert!(size_of::<F>() <= MAX_INLINE_FUTURE_BYTES, "{phase} inlines {} bytes", size_of::<F>());
}

#[test]
fn psp_owners_keep_native_phase_futures_and_retained_values_off_the_test_stack() {
    // Factories supply types only. Invoking one would require a genuine admitted fixture/owner; no fake
    // native authority or connection is constructed merely to inspect the compiled future footprint.
    assert_future_bound("fixture lifetime", |()| super::super::super::fixture::run("size_only", healthy));
    assert_future_bound("PSP case", |fixture: &'static OwnedSourceFixture| healthy(fixture, "size_only"));
    assert_future_bound(
        "Query Store read and close",
        |(pool, cancellation): (&'static Arc<crate::conn::Pool>, &'static CollectionCancellation)| {
            workload::read(pool, 0, "size_only", 0, cancellation)
        },
    );
    assert_future_bound("native capture and sealed readback", |owner: &'static mut OwnedCaptures| {
        owner.capture(CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans })
    });
    assert_future_bound("application capture and metadata close", |probe: &'static mut probe::Probe<'static>| {
        probe.record(1, RoutingExpectation::Variant, None)
    });
    assert!(size_of::<Captured>() <= MAX_INLINE_FUTURE_BYTES);
    assert!(size_of::<retained::SavedVariants>() <= MAX_INLINE_FUTURE_BYTES);
    assert!(size_of::<probe::Run>() <= MAX_INLINE_FUTURE_BYTES);
}
