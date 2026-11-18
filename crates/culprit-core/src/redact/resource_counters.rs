//! Retained counter relationships survive sharing without exposing native object/instance or arbitrary properties.
use super::{opaque, redact_native_identity};
use crate::{
    ResetMarker, Result,
    telemetry::{CollectionStop, NativeCounterSet, PERFORMANCE_COUNTER_WITHHELD_PROPERTY, PerformanceCounterProfile},
};
use std::collections::BTreeMap;

pub(super) fn redact(
    mut samples: Vec<NativeCounterSet>,
    profile: Option<&PerformanceCounterProfile>,
) -> Result<Vec<NativeCounterSet>> {
    if samples.iter().any(|sample| !sample.is_valid()) {
        return Err(CollectionStop::InvalidPage.error());
    }
    for sample in &mut samples {
        let withheld =
            sample.properties.get(PERFORMANCE_COUNTER_WITHHELD_PROPERTY).is_some_and(|value| value == "true");
        let performance = profile.filter(|profile| profile.matches(sample));
        if let Some(profile) = performance {
            profile.validate(sample)?;
        }
        let withheld = withheld || performance.is_some();
        redact_native_identity(&mut sample.identity);
        sample.identity.qualifiers =
            sample.identity.qualifiers.iter().map(|(key, value)| (opaque(key), value.clone())).collect();
        for ratio in &mut sample.ratios {
            ratio.numerator = ratio.numerator.as_deref().map(opaque);
            ratio.base = ratio.base.as_deref().map(opaque);
        }
        for counter in &mut sample.counters {
            counter.key = opaque(&counter.key);
            counter.native_object = opaque(&counter.native_object);
            counter.native_name = opaque(&counter.native_name);
            counter.native_instance = counter.native_instance.as_deref().map(opaque);
            if let Some(value) = &mut counter.native_type
                && value.parse::<i32>().ok().is_none_or(|code| code.to_string() != *value)
            {
                *value = opaque(value);
            }
            if let ResetMarker::Generation(value) = &mut counter.reset {
                *value = opaque(value);
            }
        }
        for gap in &mut sample.ratio_gaps {
            gap.key = opaque(&gap.key);
            gap.native_object = opaque(&gap.native_object);
            gap.native_name = opaque(&gap.native_name);
            gap.native_instance = gap.native_instance.as_deref().map(opaque);
        }
        let mut properties = BTreeMap::new();
        for (key, value) in std::mem::take(&mut sample.properties) {
            if key == PERFORMANCE_COUNTER_WITHHELD_PROPERTY {
                continue;
            }
            if properties.insert(opaque(&key), opaque(&value)).is_some() {
                return Err(CollectionStop::InvalidPage.error());
            }
        }
        if withheld {
            properties.insert(PERFORMANCE_COUNTER_WITHHELD_PROPERTY.into(), "true".into());
        }
        sample.properties = properties;
    }
    samples.iter().all(NativeCounterSet::is_valid).then_some(samples).ok_or_else(|| CollectionStop::InvalidPage.error())
}

#[cfg(test)]
mod tests;
