//! A source declares native category meaning once; a consumer never guesses it from an engine or source name.
use super::*;
use serde::de::{MapAccess, Visitor};
use std::{collections::BTreeMap, fmt};

pub const MAX_NATIVE_WAIT_CATEGORIES: usize = 256;
pub const MAX_NATIVE_WAIT_CATEGORY_BYTES: usize = 128;
pub const MAX_NATIVE_WAIT_NAME_BYTES: usize = 256;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitClass {
    Cpu,
    Workers,
    Rpc,
    Storage,
    Io,
    Rows,
    Lock,
    Latch,
    Log,
    Network,
    Parallelism,
    Memory,
    Compilation,
    Other,
}
impl WaitClass {
    pub const ALL: &[Self] = &[
        Self::Cpu,
        Self::Workers,
        Self::Rpc,
        Self::Storage,
        Self::Io,
        Self::Rows,
        Self::Lock,
        Self::Latch,
        Self::Log,
        Self::Network,
        Self::Parallelism,
        Self::Memory,
        Self::Compilation,
        Self::Other,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Workers => "workers",
            Self::Rpc => "rpc",
            Self::Storage => "storage",
            Self::Io => "io",
            Self::Rows => "rows",
            Self::Lock => "lock",
            Self::Latch => "latch",
            Self::Log => "log",
            Self::Network => "network",
            Self::Parallelism => "parallelism",
            Self::Memory => "memory",
            Self::Compilation => "compilation",
            Self::Other => "other",
        }
    }

    /// Stable machine categories consumed by the existing pure analysis; native names remain separate facts.
    pub fn category(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Workers => "Workers",
            Self::Rpc => "RPC",
            Self::Storage => "Storage",
            Self::Io => "IO",
            Self::Rows => "Rows",
            Self::Lock => "Lock",
            Self::Latch => "Latch",
            Self::Log => "Log",
            Self::Network => "Network",
            Self::Parallelism => "Parallelism",
            Self::Memory => "Memory",
            Self::Compilation => "Compilation",
            Self::Other => "Other",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitCaptureState {
    Enabled,
    Disabled,
    Unavailable,
}

/// Current capture mode does not establish whether a historical interval retained its complete wait population.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitCaptureCoverage {
    Unverified,
    /// The source establishes capture over this interval and measurement population. Readback still needs proof.
    Qualified,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "class", rename_all = "kebab-case")]
#[ts(export)]
pub enum WaitDisposition {
    Analyze(WaitClass),
    Ignore,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeWaitMeaning {
    /// A native technical name, not a localized label and never an individual wait type inferred from a category.
    pub native_name: String,
    pub disposition: WaitDisposition,
}
impl NativeWaitMeaning {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.native_name) && self.native_name.len() <= MAX_NATIVE_WAIT_NAME_BYTES
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct QueryWaitSemantics {
    pub capture: WaitCaptureState,
    pub coverage: WaitCaptureCoverage,
    /// Exact source-native IDs. An absent entry has unknown meaning, never the analytical Other class.
    #[serde(deserialize_with = "deserialize_categories")]
    pub categories: BTreeMap<String, NativeWaitMeaning>,
}
impl QueryWaitSemantics {
    pub fn is_valid(&self) -> bool {
        self.categories.len() <= MAX_NATIVE_WAIT_CATEGORIES
            && (self.capture != WaitCaptureState::Enabled || !self.categories.is_empty())
            && self.categories.iter().all(|(id, meaning)| valid_category_id(id) && meaning.is_valid())
    }
}

fn valid_category_id(id: &str) -> bool {
    valid_key(id) && id.len() <= MAX_NATIVE_WAIT_CATEGORY_BYTES
}

/// Reject duplicate IDs and excess entries during decoding, before a map can erase a conflicting declaration.
fn deserialize_categories<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, NativeWaitMeaning>, D::Error> {
    struct Categories;
    impl<'de> Visitor<'de> for Categories {
        type Value = BTreeMap<String, NativeWaitMeaning>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded dictionary of distinct native wait categories")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> std::result::Result<Self::Value, A::Error> {
            let mut categories = BTreeMap::new();
            while let Some(id) = input.next_key::<String>()? {
                if categories.len() == MAX_NATIVE_WAIT_CATEGORIES || !valid_category_id(&id) {
                    return Err(serde::de::Error::custom("invalid or duplicate native wait category"));
                }
                let std::collections::btree_map::Entry::Vacant(entry) = categories.entry(id) else {
                    return Err(serde::de::Error::custom("duplicate native wait category"));
                };
                let meaning = input.next_value::<NativeWaitMeaning>()?;
                if !meaning.is_valid() {
                    return Err(serde::de::Error::custom("invalid native wait name"));
                }
                entry.insert(meaning);
            }
            Ok(categories)
        }
    }
    deserializer.deserialize_map(Categories)
}

#[cfg(test)]
mod tests;
