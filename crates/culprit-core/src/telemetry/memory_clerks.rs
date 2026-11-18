//! Presence detection selects a native family; it never authorizes interpreting malformed counter values.
use super::{NativeCounterSet, StateSourceCoverage};

pub const MEMORY_CLERK_NATIVE_OBJECT: &str = "sys.dm_os_memory_clerks";

pub fn memory_clerk_candidate(sample: &NativeCounterSet) -> bool {
    sample.identity.qualifiers.get("native_object").is_some_and(|value| value == MEMORY_CLERK_NATIVE_OBJECT)
        || sample.counters.iter().any(|counter| counter.native_object == MEMORY_CLERK_NATIVE_OBJECT)
        || sample.properties.get("aggregation").is_some_and(|value| value == "clerk-type-name-and-memory-node")
}

pub fn memory_clerk_source_is_valid(source: &StateSourceCoverage) -> bool {
    source.source == crate::source_id::MEMORY_CLERKS && source.is_valid()
}

impl super::CapturedOperationalRead {
    /// The original shared instance owns this family. A selected database or current member never replaces it.
    pub fn memory_clerk_source(&self) -> crate::Result<&StateSourceCoverage> {
        if !self.is_valid()
            || !matches!(&self.description.target, super::CollectionTarget::Shared(resource)
                if resource.kind == super::SharedResourceKind::Instance)
        {
            return Err(super::CollectionStop::InvalidPage.error());
        }
        self.source_coverage(crate::source_id::MEMORY_CLERKS).ok_or_else(|| super::CollectionStop::InvalidPage.error())
    }
}
