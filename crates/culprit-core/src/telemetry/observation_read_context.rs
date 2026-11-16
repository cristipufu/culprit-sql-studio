//! Read clocks retain their original authority; collector timing never establishes native scope or continuity.
use super::*;
use std::time::SystemTime;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum ObservationReadContext {
    /// The original source descriptor supplies these native read bounds and its independent scope proof.
    Native { context: Box<NativeReadContext> },
    /// Collector-clock read bounds do not establish native event time, member continuity or connection shutdown.
    Collector { observed_from: String, observed_until: String },
}

impl ObservationReadContext {
    pub fn collector(from: SystemTime, until: SystemTime) -> Result<Self> {
        let context = Self::Collector { observed_from: collector_time(from)?, observed_until: collector_time(until)? };
        context.is_valid().then_some(context).ok_or_else(invalid)
    }

    pub fn bounds(&self) -> (&str, &str) {
        match self {
            Self::Native { context } => (&context.observed_from, &context.observed_until),
            Self::Collector { observed_from, observed_until } => (observed_from, observed_until),
        }
    }

    pub fn native(&self) -> Option<&NativeReadContext> {
        match self {
            Self::Native { context } => Some(context),
            Self::Collector { .. } => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        let (from, until) = self.bounds();
        from.len() <= 64
            && until.len() <= 64
            && ObservationBounds::new(from, until).is_some()
            && self.native().is_none_or(NativeReadContext::is_valid)
    }
}

fn collector_time(value: SystemTime) -> Result<String> {
    let elapsed = value.duration_since(SystemTime::UNIX_EPOCH).map_err(|_| invalid())?;
    let seconds = i64::try_from(elapsed.as_secs()).map_err(|_| invalid())?;
    let value = chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, elapsed.subsec_nanos()).ok_or_else(invalid)?;
    Ok(value.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
