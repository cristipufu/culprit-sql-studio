//! Exact same-read cache inputs stay separate from pressure grades and selected-database measurements.
use super::*;
use crate::{deployment::NativeDeployment, value::Rec};
use culprit_core::telemetry::{
    CounterRatioGap, CounterRatioKind, CounterRatioSupport, NativeCounterRatio, NativeCounterSet, NativeIdentity,
    ObservationBounds,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct PerformanceRead {
    pub(crate) sample: NativeCounterSet,
    pub(crate) native_rows: usize,
    pub(crate) resource_governor: Vec<NativeCounterSet>,
    pub(crate) resource_governor_complete: bool,
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
    population: Population<'_>,
) -> Result<PerformanceRead> {
    let metadata = ReadMetadata::decode(rows.first().ok_or_else(invalid_data)?);
    if rows.iter().skip(1).any(|row| ReadMetadata::decode(row) != metadata) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let mut populations =
        BTreeMap::<(CounterRatioKind, String, Option<String>), (Option<String>, Option<String>)>::new();
    for (key, object, name, instance) in counters
        .iter()
        .map(|row| (&row.key, &row.native_object, &row.native_name, &row.native_instance))
        .chain(ratio_gaps.iter().map(|row| (&row.key, &row.native_object, &row.native_name, &row.native_instance)))
    {
        let Some(spec) = population.field(object, name, instance.as_deref()) else {
            return Err(invalid_data());
        };
        if let Some(kind) = spec.ratio {
            let pair = populations.entry((kind, object.clone(), instance.clone())).or_default();
            match spec.nature {
                CounterNature::FractionNumerator | CounterNature::AverageNumerator => pair.0 = Some(key.clone()),
                CounterNature::FractionBase => pair.1 = Some(key.clone()),
                _ => return Err(invalid_data()),
            }
        }
    }
    let mut ratios = Vec::new();
    for ((kind, _, _), (numerator, base)) in &populations {
        ratios.push(NativeCounterRatio {
            kind: *kind,
            numerator: numerator.clone(),
            base: base.clone(),
            support: if *kind == CounterRatioKind::LocalSsdCacheHit {
                metadata.support()
            } else {
                CounterRatioSupport::Supported
            },
            observed_at: metadata.at.clone(),
        });
    }
    if !populations.keys().any(|(kind, _, _)| *kind == CounterRatioKind::LocalSsdCacheHit)
        && metadata.support() == CounterRatioSupport::Supported
    {
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
    if matches!(population, Population::InstanceInvestigation)
        && !populations.keys().any(|(kind, _, _)| *kind == CounterRatioKind::BufferCacheHit)
    {
        ratios.push(NativeCounterRatio {
            kind: CounterRatioKind::BufferCacheHit,
            numerator: None,
            base: None,
            support: CounterRatioSupport::Supported,
            observed_at: metadata.at.clone(),
        });
    }
    if matches!(population, Population::InstanceInvestigation)
        && !populations.keys().any(|(kind, _, _)| *kind == CounterRatioKind::LockWaitAverageInputs)
    {
        ratios.push(NativeCounterRatio {
            kind: CounterRatioKind::LockWaitAverageInputs,
            numerator: None,
            base: None,
            support: CounterRatioSupport::Supported,
            observed_at: metadata.at.clone(),
        });
    }
    let profile = culprit_core::Engine::Mssql.spec().performance_counters().ok_or_else(invalid_data)?;
    let sample = NativeCounterSet {
        identity: NativeIdentity::new(profile.native_object).qualified("native_object", profile.native_object),
        scope: MeasurementScope::Server,
        counters,
        ratios,
        ratio_gaps,
        properties: {
            let mut properties = BTreeMap::from([
                (culprit_core::telemetry::PERFORMANCE_COUNTER_PROFILE_PROPERTY.into(), population.selection().into()),
                (culprit_core::telemetry::PERFORMANCE_COUNTER_ROWS_PROPERTY.into(), rows.len().to_string()),
            ]);
            if let Some(at) = &metadata.at {
                properties.insert(
                    culprit_core::telemetry::PERFORMANCE_COUNTER_CLOCK_PROPERTY.into(),
                    culprit_core::telemetry::canonical_time(at)?,
                );
            }
            properties
        },
    };
    profile.validate(&sample)?;
    // The native witness is the original raw row count, independently of one aggregate archive observation.
    Ok(PerformanceRead {
        sample,
        native_rows: rows.len(),
        resource_governor: Vec::new(),
        resource_governor_complete: true,
    })
}
