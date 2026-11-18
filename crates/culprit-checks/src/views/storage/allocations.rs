use culprit_core::{Result, Snapshot, operations::SpaceUsage, telemetry::*};
use std::collections::BTreeMap;

/// The active chart is a projection; an attached origin must still identify its original exact allocation.
pub(crate) struct AllocationEvidence<'a> {
    spaces: BTreeMap<(&'a str, &'a str), &'a OperationalSpace>,
}

impl<'a> AllocationEvidence<'a> {
    pub(crate) fn from_snapshot(snapshot: &'a Snapshot) -> Result<Self> {
        let mut spaces = BTreeMap::new();
        if let Some(read) = &snapshot.operations.storage {
            if !read.is_valid() {
                return Err(CollectionStop::InvalidPage.error());
            }
            for row in &read.observations {
                if let OperationalFact::Space(space) = &row.fact {
                    spaces.insert((row.source.as_str(), row.key.as_str()), space);
                }
            }
        }
        let evidence = Self { spaces };
        for projected in &snapshot.operations.spaces {
            if projected.origin.is_some()
                && evidence.space(projected).is_none_or(|native| !matches_projection(native, projected))
            {
                return Err(CollectionStop::InvalidPage.error());
            }
        }
        Ok(evidence)
    }

    pub(crate) fn space(&self, projected: &SpaceUsage) -> Option<&'a OperationalSpace> {
        let reference = projected.origin.as_ref()?;
        self.spaces.get(&(reference.source.as_str(), reference.key.as_str())).copied()
    }
}

fn matches_projection(native: &OperationalSpace, projected: &SpaceUsage) -> bool {
    let bytes = |value: &Option<String>| value.as_deref().and_then(|value| value.parse::<u64>().ok()).map(|v| v as f64);
    native.kind == projected.kind
        && native.scope == projected.scope
        && bytes(&native.total_bytes) == Some(projected.total_bytes)
        && bytes(&native.used_bytes) == Some(projected.used_bytes)
        && bytes(&native.limit_bytes) == projected.limit_bytes
        && native.growth_enabled == projected.growth_enabled
        && native.reuse_wait == projected.reuse_wait
}
