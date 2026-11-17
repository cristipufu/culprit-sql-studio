//! Synthetic source-task accounting for metadata replay; no SQL is sent or native cost qualified.
use culprit_core::{
    Engine,
    telemetry::{ReadMetricsRecorder, ReadProgress, ReadTaskOutcome, ReadWorkMetrics},
};
use std::{sync::atomic::Ordering, time::Duration};

pub const SOURCE_OVERHEAD_QUERY_COUNT: u32 = 100;

pub fn source_overhead_read_metrics() -> ReadWorkMetrics {
    let recorder = ReadMetricsRecorder::new(Engine::Postgres);
    let progress = ReadProgress::default();
    progress.rows.store(u64::MAX, Ordering::Relaxed);
    progress.bytes.store(9_007_199_254_740_993, Ordering::Relaxed);
    for index in 0..SOURCE_OVERHEAD_QUERY_COUNT {
        let id = recorder
            .begin(&format!("/* culprit */ SELECT native_value FROM overhead_preview_canary WHERE native_id={index}"));
        let outcome = ReadTaskOutcome { failed: index == 3, timed_out: index == 3, ..Default::default() };
        recorder.finish(id.as_deref(), Duration::from_micros(u64::from(index + 1) * 1_000 + 125), &progress, &outcome);
    }
    let metrics = recorder.snapshot().expect("synthetic recorder lock is not poisoned");
    assert!(metrics.is_valid());
    metrics
}
