//! Same-row resource details retain role and measurements without publishing a physical member identifier.
use super::opaque;
use crate::{ResourceSample, Result, telemetry::CollectionStop};

pub(super) fn redact(mut samples: Vec<ResourceSample>) -> Result<Vec<ResourceSample>> {
    if samples.iter().any(|sample| !sample.has_consistent_measurement()) {
        return Err(CollectionStop::InvalidPage.error());
    }
    for sample in &mut samples {
        if let Some(measurement) = &mut sample.measurement {
            measurement.replica_id = measurement.replica_id.as_deref().map(opaque);
            for metric in &mut measurement.metrics {
                if !public_metric_key(&metric.key) {
                    metric.key = opaque(&metric.key);
                }
            }
        }
    }
    samples
        .iter()
        .all(ResourceSample::has_consistent_measurement)
        .then_some(samples)
        .ok_or_else(|| CollectionStop::InvalidPage.error())
}

// These nine machine keys describe the eleven canonical database-resource dimensions. CPU and memory
// each also have a separately scoped server value; a public schema name grants neither scope nor ownership.
fn public_metric_key(key: &str) -> bool {
    matches!(
        key,
        "cpu" | "data-io" | "log-write" | "memory" | "workers" | "sessions" | "xtp-storage" | "dtu-limit" | "cpu-limit"
    )
}
