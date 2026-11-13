use super::super::recorded_waits::{LockTotal, RecordedLocks, WaitSource};
use culprit_core::{QueryStat, Snapshot, format, t, telemetry::WaitClass};

/// Aggregate waits qualify an observation, not which calls were slow or whether their parameters mattered.
pub(super) enum Recorded {
    Incomplete,
    ObservedLocks { source: WaitSource, lock_ms: f64 },
    Measured { source: WaitSource, lock_ms: f64, other: Option<OtherWait> },
}

pub(super) struct OtherWait {
    native_name: Option<String>,
    ms: f64,
    meaning: Meaning,
}

#[derive(Clone, Copy)]
enum Meaning {
    Network,
    Io,
    Memory,
}
impl Meaning {
    fn class(self) -> WaitClass {
        match self {
            Self::Network => WaitClass::Network,
            Self::Io => WaitClass::Io,
            Self::Memory => WaitClass::Memory,
        }
    }

    fn sentence(self) -> String {
        match self {
            Self::Network => t!("check.unstable_query.meaning.network"),
            Self::Io => t!("check.unstable_query.meaning.io"),
            Self::Memory => t!("check.unstable_query.meaning.memory"),
        }
    }
}
impl OtherWait {
    fn read(query: &QueryStat) -> Option<Self> {
        // Select the largest observed non-CPU/non-lock class before choosing wording; a smaller readable class
        // cannot replace a larger class this sentence cannot explain.
        if query.waits.iter().any(|wait| culprit_core::waits::known_category_key(&wait.category).is_none()) {
            return None;
        }
        let (class, ms) = WaitClass::ALL
            .iter()
            .filter(|class| !matches!(class, WaitClass::Lock | WaitClass::Cpu))
            .filter_map(|class| Some((*class, query.observed_wait_ms(*class).filter(|ms| *ms > 0.0)?)))
            .max_by(|(_, a), (_, b)| a.total_cmp(b))?;
        let meaning = match class {
            WaitClass::Network => Meaning::Network,
            WaitClass::Io => Meaning::Io,
            WaitClass::Memory => Meaning::Memory,
            _ => return None,
        };
        let native_name = query
            .waits
            .iter()
            .filter(|wait| culprit_core::waits::known_category_key(&wait.category) == Some(class.key()))
            .max_by(|a, b| a.wait_ms.total_cmp(&b.wait_ms))
            .and_then(|wait| wait.wait_type.clone());
        Some(Self { native_name, ms, meaning })
    }

    fn name(&self, snapshot: &Snapshot) -> String {
        self.native_name
            .clone()
            .unwrap_or_else(|| culprit_core::waits::category_info(self.meaning.class().key(), snapshot.server.engine).0)
    }
}

impl Recorded {
    pub fn read(snapshot: &Snapshot, query: &QueryStat) -> Self {
        match RecordedLocks::read(snapshot, query) {
            Some(RecordedLocks { source, total: LockTotal::Measured(lock_ms) }) => {
                Self::Measured { source, lock_ms, other: OtherWait::read(query) }
            }
            Some(RecordedLocks { source, total: LockTotal::Observed(lock_ms) }) => {
                Self::ObservedLocks { source, lock_ms }
            }
            None => Self::Incomplete,
        }
    }

    pub fn has_positive_locks(&self) -> bool {
        match self {
            Self::ObservedLocks { lock_ms, .. } | Self::Measured { lock_ms, .. } => *lock_ms > 0.0,
            Self::Incomplete => false,
        }
    }

    pub fn sentence(&self, snapshot: &Snapshot, excess: f64) -> String {
        let values = t!("check.unstable_query.recorded.compare_values");
        let extra = format::duration(excess);
        match self {
            Self::Incomplete => [t!("check.unstable_query.recorded.open_incomplete"), values].join(" "),
            Self::ObservedLocks { source, lock_ms } => [
                t!(
                    "check.unstable_query.recorded.locks_observed",
                    source = source.label(snapshot),
                    lock = format::duration(*lock_ms),
                    extra = extra
                ),
                values,
            ]
            .join(" "),
            Self::Measured { source, lock_ms, other } => {
                let source = source.label(snapshot);
                if *lock_ms >= excess * 0.5 {
                    return [
                        t!(
                            "check.unstable_query.recorded.locks",
                            source = source,
                            lock = format::duration(*lock_ms),
                            extra = extra
                        ),
                        values,
                    ]
                    .join(" ");
                }
                let locks = match (*lock_ms == 0.0, other) {
                    (true, None) => t!("check.unstable_query.recorded.no_locks", source = source),
                    (true, Some(wait)) => t!(
                        "check.unstable_query.recorded.no_locks_other",
                        source = source,
                        wait = wait.name(snapshot),
                        ms = format::duration(wait.ms),
                        meaning = wait.meaning.sentence()
                    ),
                    (false, None) => t!(
                        "check.unstable_query.recorded.few_locks",
                        source = source,
                        lock = format::duration(*lock_ms),
                        extra = extra
                    ),
                    (false, Some(wait)) => t!(
                        "check.unstable_query.recorded.few_locks_other",
                        source = source,
                        lock = format::duration(*lock_ms),
                        extra = extra,
                        wait = wait.name(snapshot),
                        ms = format::duration(wait.ms),
                        meaning = wait.meaning.sentence()
                    ),
                };
                [locks, values].join(" ")
            }
        }
    }
}
