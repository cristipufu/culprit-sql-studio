//! Exact same-read cache inputs stay separate from pressure grades and selected-database measurements.
use super::*;
use crate::{deployment::NativeDeployment, value::Rec};
use culprit_core::telemetry::{
    CounterRatioGap, CounterRatioKind, CounterRatioSupport, NativeCounterRatio, NativeCounterSet, NativeIdentity,
    ObservationBounds,
};
use std::collections::BTreeMap;

const NUMERATOR: &str = "RBPEX cache hit ratio";
const BASE: &str = "RBPEX cache hit ratio base";

#[derive(Clone, Debug)]
pub(crate) struct PerformanceRead {
    pub(crate) sample: NativeCounterSet,
    pub(crate) native_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReadMetadata {
    deployment: Option<NativeDeployment>,
    at: Option<String>,
}
impl ReadMetadata {
    fn decode(row: &Rec) -> Self {
        // Optional ratio metadata is decoded independently. A missing clock never erases a known deployment,
        // and malformed optional metadata cannot authorize a ratio or erase ordinary native counter rows.
        let deployment = NativeDeployment::decode(row).ok();
        let at = text(row, "counter_observed_at")
            .ok()
            .filter(|at| at.len() <= 64 && ObservationBounds::new(at, at).is_some());
        Self { deployment, at }
    }
    fn support(&self) -> CounterRatioSupport {
        match &self.deployment {
            Some(deployment) if deployment.hyperscale() => CounterRatioSupport::Supported,
            Some(deployment) if deployment.conventional() => CounterRatioSupport::Unsupported,
            _ => CounterRatioSupport::Unknown,
        }
    }
}

pub(super) fn captured(
    rows: &[Rec],
    counters: Vec<NativeCounterSample>,
    ratio_gaps: Vec<CounterRatioGap>,
) -> Result<PerformanceRead> {
    let metadata = ReadMetadata::decode(rows.first().ok_or_else(invalid_data)?);
    if rows.iter().skip(1).any(|row| ReadMetadata::decode(row) != metadata) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let mut populations = BTreeMap::<(String, Option<String>), BTreeMap<String, String>>::new();
    for (key, object, name, instance) in counters
        .iter()
        .map(|row| (&row.key, &row.native_object, &row.native_name, &row.native_instance))
        .chain(ratio_gaps.iter().map(|row| (&row.key, &row.native_object, &row.native_name, &row.native_instance)))
    {
        if name == NUMERATOR || name == BASE {
            populations.entry((object.clone(), instance.clone())).or_default().insert(name.clone(), key.clone());
        }
    }
    let mut ratios = Vec::new();
    for names in populations.values() {
        ratios.push(NativeCounterRatio {
            kind: CounterRatioKind::LocalSsdCacheHit,
            numerator: names.get(NUMERATOR).cloned(),
            base: names.get(BASE).cloned(),
            support: metadata.support(),
            observed_at: metadata.at.clone(),
        });
    }
    if populations.is_empty() && metadata.support() == CounterRatioSupport::Supported {
        // This is an expectation of the genuine fully drained source read. Neither input claims a native row or
        // object/instance existed. The read has its actual ordinary counters; no empty synthetic object is emitted.
        ratios.push(NativeCounterRatio {
            kind: CounterRatioKind::LocalSsdCacheHit,
            numerator: None,
            base: None,
            support: CounterRatioSupport::Supported,
            observed_at: metadata.at.clone(),
        });
    }
    let sample = NativeCounterSet {
        identity: NativeIdentity::new("sys.dm_os_performance_counters")
            .qualified("native_object", "sys.dm_os_performance_counters"),
        scope: MeasurementScope::Server,
        counters,
        ratios,
        ratio_gaps,
        properties: BTreeMap::new(),
    };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    // The native witness is the original raw row count, independently of one aggregate archive observation.
    Ok(PerformanceRead { sample, native_rows: rows.len() })
}
