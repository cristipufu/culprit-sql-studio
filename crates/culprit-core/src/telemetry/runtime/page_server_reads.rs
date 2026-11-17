//! Native remote-page statistics never become local-cache misses, elapsed time or physical-member demand.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageServerReadAvailability {
    Available,
    NotApplicable,
    /// The native profile is Hyperscale, but the required fields were not positively visible.
    Unavailable,
    #[default]
    Unknown,
}
impl PageServerReadAvailability {
    pub const PROPERTY: &str = "page_server_reads";

    pub fn native_property(self) -> &'static str {
        match self {
            Self::Available => "native_hyperscale_statistics",
            Self::NotApplicable => "not_applicable",
            Self::Unavailable => "native_columns_unavailable",
            Self::Unknown => "availability_unknown",
        }
    }
    pub fn from_native_property(value: &str) -> Option<Self> {
        [Self::Available, Self::NotApplicable, Self::Unavailable, Self::Unknown]
            .into_iter()
            .find(|availability| availability.native_property() == value)
    }
}

/// Statistics in 8-KiB pages. Native fragment averages/deviations preserve float precision and last/min/max
/// preserve bigint. A separately tagged derived summary admits only its same-plan population's supported subset.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PageServerReadStatistics {
    pub availability: PageServerReadAvailability,
    pub average_pages: Option<f64>,
    pub last_pages: Option<String>,
    pub minimum_pages: Option<String>,
    pub maximum_pages: Option<String>,
    pub standard_deviation_pages: Option<f64>,
}
impl PageServerReadStatistics {
    pub const BYTES_PER_PAGE: u32 = 8192;

    pub fn is_valid(&self) -> bool {
        [self.average_pages, self.standard_deviation_pages]
            .into_iter()
            .flatten()
            .all(|value| value.is_finite() && value >= 0.0)
            && [&self.last_pages, &self.minimum_pages, &self.maximum_pages]
                .into_iter()
                .flatten()
                .all(|value| PageServerReadStatistics::native_count(value).is_some())
            && (self.availability == PageServerReadAvailability::Available || !self.has_values())
    }
    pub fn has_values(&self) -> bool {
        self.average_pages.is_some()
            || self.last_pages.is_some()
            || self.minimum_pages.is_some()
            || self.maximum_pages.is_some()
            || self.standard_deviation_pages.is_some()
    }
}

/// A scan retains per-plan statistics within its selected execution group/window. It never averages plans or
/// replica groups. Multiple native fragments authorize no representative last or pooled standard deviation.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum PageServerReadSummary {
    NativeFragment {
        /// Original native readers retain this identity. Sharing removes lookup authority while preserving
        /// independently valid statistics; it never creates a new original runtime reference.
        native: Option<NativeIdentity>,
        executions: String,
        statistics: Box<PageServerReadStatistics>,
    },
    Derived {
        native_fragments: String,
        executions: String,
        /// None means the existing plan summary contains different native execution outcomes. It authorizes
        /// no combined remote-read statistic; the original fragments remain independently readable.
        execution_type: Option<String>,
        statistics: Box<PageServerReadStatistics>,
    },
}
impl PageServerReadSummary {
    pub fn executions(&self) -> &str {
        match self {
            Self::NativeFragment { executions, .. } | Self::Derived { executions, .. } => executions,
        }
    }
    pub fn statistics(&self) -> &PageServerReadStatistics {
        match self {
            Self::NativeFragment { statistics, .. } | Self::Derived { statistics, .. } => statistics,
        }
    }
    pub fn is_valid(&self) -> bool {
        self.statistics().is_valid()
            && PageServerReadStatistics::native_count(self.executions()).is_some()
            && match self {
                Self::NativeFragment { native, .. } => native.as_ref().is_none_or(NativeIdentity::is_valid),
                Self::Derived { native_fragments, execution_type, statistics, .. } => {
                    PageServerReadStatistics::native_count(native_fragments).is_some_and(|count| count >= 2)
                        && execution_type.as_deref().is_none_or(|value| {
                            PageServerReadStatistics::native_count(value).is_some_and(|kind| kind <= u8::MAX as i64)
                        })
                        && (execution_type.is_some() || !statistics.has_values())
                        && statistics.last_pages.is_none()
                        && statistics.standard_deviation_pages.is_none()
                }
            }
    }
}

impl PageServerReadStatistics {
    /// Canonical nonnegative native bigint, without floating-point conversion.
    pub fn native_count(value: &str) -> Option<i64> {
        (value.len() <= 19)
            .then(|| value.parse::<i64>().ok())
            .flatten()
            .filter(|number| *number >= 0 && number.to_string() == value)
    }
}

#[cfg(test)]
mod tests;
