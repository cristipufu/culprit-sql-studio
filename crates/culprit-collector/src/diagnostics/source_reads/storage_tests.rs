//! Inspect actual production future/output types without creating a native client or polling a future.
use super::*;
use std::{future::Future, mem::size_of};

// The exact crashed binary stored 30,920 bytes in analyze_pending and 26,072 in parents; its poll frames
// reserved 317,200 and 133,024 bytes respectively. These are caller-storage limits, not native-work budgets.
const MAX_QUEUE_FUTURE_BYTES: usize = 16 * 1024;
const MAX_INPUT_FUTURE_BYTES: usize = 48 * 1024;
const MAX_PARENT_FUTURE_BYTES: usize = 28 * 1024;
const MAX_PARENT_OUTPUT_BYTES: usize = 512;

type QueueArgs = (
    &'static mut Outbox,
    &'static DiagnosticPolicies,
    &'static CollectionCancellation,
    u32,
    Option<&'static dyn RecoveryStore>,
);
type InputArgs = (
    &'static mut Outbox,
    Box<DiagnosticInput>,
    &'static DiagnosticPolicies,
    Option<&'static dyn RecoveryStore>,
    &'static CollectionCancellation,
);
type ParentArgs = (
    &'static mut Outbox,
    &'static DiagnosticInput,
    Option<&'static dyn RecoveryStore>,
    &'static CollectionCancellation,
);
fn storage<Args, F, Fut>(_: F) -> (usize, usize)
where
    F: FnOnce(Args) -> Fut,
    Fut: Future,
{
    (size_of::<Fut>(), size_of::<Fut::Output>())
}

#[test]
fn actual_queue_and_per_input_future_types_do_not_embed_the_whole_recovery_controller_in_each_caller() {
    let queue = storage(|args: QueueArgs| analyze_pending(args.0, args.1, args.2, args.3, args.4));
    // This is the underlying concrete async helper, not its Pin<Box<_>> or a dyn Future handle.
    let input = storage(|args: InputArgs| analyze_input(args.0, args.1, args.2, args.3, args.4));
    eprintln!("Diagnostic future storage: queue={}B input={}B; outputs={}B/{}B", queue.0, input.0, queue.1, input.1);
    assert!(queue.0 <= MAX_QUEUE_FUTURE_BYTES, "actual bounded queue frame: {}", queue.0);
    assert!(input.0 <= MAX_INPUT_FUTURE_BYTES, "actual underlying per-input frame: {}", input.0);
    assert_eq!(queue.1, size_of::<Result<u32>>());
    assert_eq!(input.1, size_of::<Result<bool>>());
}

#[test]
fn actual_original_parent_future_keeps_its_large_native_body_boxed_across_output_and_later_awaits() {
    let parent = storage(|args: ParentArgs| parents(args.0, args.1, args.2, args.3));
    eprintln!(
        "Diagnostic original storage: future={}B output={}B body={}B",
        parent.0,
        parent.1,
        size_of::<OriginalBasis>()
    );
    assert!(parent.0 <= MAX_PARENT_FUTURE_BYTES, "actual original-parent future: {}", parent.0);
    assert_eq!(parent.1, size_of::<Result<Option<Box<OriginalBasis>>>>());
    assert!(parent.1 <= MAX_PARENT_OUTPUT_BYTES, "the native original cannot be copied through Poll outputs");
    assert!(size_of::<OriginalBasis>() > MAX_PARENT_OUTPUT_BYTES, "the output guard must cover the actual large body");
}
