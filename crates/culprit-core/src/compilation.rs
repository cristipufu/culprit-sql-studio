//! Compiler working memory is released after compilation. These retained statistics are neither current ownership
//! nor plan-cache size, and multiplying them by a compilation count cannot establish simultaneous memory use.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq)]
#[ts(export)]
pub struct CompilationMemory {
    pub average_kb: Option<f64>,
    pub last_kb: Option<f64>,
    pub maximum_kb: Option<f64>,
}

impl CompilationMemory {
    pub fn is_valid(&self) -> bool {
        [self.average_kb, self.last_kb, self.maximum_kb]
            .into_iter()
            .flatten()
            .all(|value| value.is_finite() && value >= 0.0)
    }

    pub fn is_measured(&self) -> bool {
        self.average_kb.is_some() || self.last_kb.is_some() || self.maximum_kb.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_memory_is_distinct_from_zero_and_invalid_native_values_are_rejected() {
        let mut memory = CompilationMemory::default();
        assert!(memory.is_valid());
        assert!(!memory.is_measured());
        memory.last_kb = Some(0.0);
        assert!(memory.is_valid() && memory.is_measured());
        for invalid in [-1.0, f64::INFINITY, f64::NAN] {
            memory.average_kb = Some(invalid);
            assert!(!memory.is_valid());
        }
    }
}
